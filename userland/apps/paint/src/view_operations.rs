//! The Adjustments and the Effects panes in the window: one preview across
//! both, its overlay worked out on the preview worker and landed only on the
//! state it was asked of; the histogram the panes read; applying an
//! operation, at once from a whole overlay or completed on the ordered queue;
//! and the input the two panes take, through the scroll of a body their dock
//! scrolls.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_controls::{damage, Keystroke};
use tairix_geometry::{Point, Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_theme::Theme;
use tairix_window::docapp::Relayout;

use super::input::Keyboard;
use super::previews::Asking;
use super::{Action, Compute, Lands, Outcome, Settles, Then, View};
use crate::canvas::{Canvas, Kind, OutOfMemory, Tile};
use crate::histogram::Histogram;
use crate::layout::Faces;
use crate::op::{Family, Op};
use crate::op_pane::{OpOutcome, OpPane};
use crate::pane::PaneKind;
use crate::preview::{Overlay, PreviewWork};
use crate::shape::{Bounds, Point as Fx, FX};

/// Why a preview could not be had.
const NO_ROOM_TO_PREVIEW: &str = "There is not enough memory to preview that";

/// Why an operation could not be applied.
const NO_ROOM_TO_APPLY: &str = "There is not enough memory to adjust the picture";

/// What applying an operation is said not to have managed.
const APPLYING: &str = "adjust the picture";

/// What an answer for the operation is worked from: the layer painted on, by
/// its revision — one no other layer of the document holds, raised whenever
/// its pixels change — and the selection's epoch. How the layers show, their
/// names and their order are not read, so a change to them keeps what was
/// worked out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Basis {
    revision: u64,
    selection: u64,
}

/// What the picture shows of the open operation, and the histogram the panes
/// read.
#[derive(Debug, Default)]
pub(crate) struct Looks {
    /// The layer as the open operation leaves it, as far as that has been
    /// worked out.
    overlay: Option<Overlay<Basis>>,
    /// The operation and state whose overlay memory pressure let go: none is
    /// made again until one of them moves.
    dropped: Option<(Op, Basis)>,
    /// A palette picture's kind with its palette mapped by the open
    /// operation, and what it was mapped by over which state: no kind where
    /// its room was refused, so none is asked again until one of them moves.
    kind: Option<(Option<Kind>, Op, Basis)>,
    /// The last histogram worked out, and the state it read.
    histogram: Option<(Histogram, Basis)>,
    /// The operation a worker is applying, shown over the picture until it
    /// lands.
    applying: Option<Op>,
}

impl View {
    /// What an answer for the operation is worked from now.
    pub(super) fn basis(&self) -> Basis {
        let revision = self
            .document
            .picture()
            .and_then(|picture| picture.layers().get(picture.active()))
            .map_or(0, |layer| layer.revision);
        Basis {
            revision,
            selection: self.selection_epoch,
        }
    }

    /// Pane `kind`, the Adjustments or the Effects pane, for the painter; the
    /// Adjustments pane for any other.
    #[must_use]
    pub(crate) fn operation_pane(&self, kind: PaneKind) -> &OpPane {
        &self.operations[kind.operation().unwrap_or(0)]
    }

    fn operation_mut(&mut self, kind: PaneKind) -> &mut OpPane {
        &mut self.operations[kind.operation().unwrap_or(0)]
    }

    /// The pane an operation is open in, if one is.
    #[must_use]
    pub(crate) fn open_operation(&self) -> Option<PaneKind> {
        PaneKind::OPERATIONS
            .into_iter()
            .find(|&kind| self.operation_pane(kind).is_open())
    }

    /// The open operation, as set.
    #[must_use]
    pub fn adjusting(&self) -> Option<Op> {
        self.open_operation()
            .map(|kind| self.operation_pane(kind).op())
    }

    /// The pane whose eyedropper is up, if one is: the next press on the
    /// picture is its.
    pub(super) fn picking_operation(&self) -> Option<PaneKind> {
        PaneKind::OPERATIONS
            .into_iter()
            .find(|&kind| self.operation_pane(kind).picking().is_some())
    }

    /// Whether a list one of the two panes opened, or a press held on one,
    /// owns the pointer.
    pub(super) fn operation_holding(&self) -> bool {
        self.operations
            .iter()
            .any(|pane| pane.listing() || pane.holding())
    }

    /// The histogram the panes draw: the last worked out, which may lag the
    /// picture by the one being worked out now.
    #[must_use]
    pub(crate) fn histogram(&self) -> Option<&Histogram> {
        self.looks
            .histogram
            .as_ref()
            .map(|(histogram, _)| histogram)
    }

    /// The operation the picture shows: one a worker is applying, else the
    /// open one while Preview is on and it changes something.
    fn shown_op(&self) -> Option<Op> {
        if self.pending.is_some() {
            return self.looks.applying;
        }
        let pane = self.operation_pane(self.open_operation()?);
        (pane.previewing() && !pane.op().is_identity()).then_some(pane.op())
    }

    /// The layer as the operation shown leaves it, as far as that is worked
    /// out over the layer as it stands.
    #[must_use]
    pub(crate) fn preview_canvas(&self) -> Option<&Canvas> {
        let op = self.shown_op()?;
        let overlay = self.looks.overlay.as_ref()?;
        overlay.shows(self.basis(), &op).then_some(overlay.shown())
    }

    /// A palette picture's kind as the operation shown maps its palette.
    #[must_use]
    pub(crate) fn preview_kind(&self) -> Option<&Kind> {
        let op = self.shown_op()?;
        let basis = self.basis();
        self.looks
            .kind
            .as_ref()
            .filter(|(_, mapped, at)| *mapped == op && *at == basis)
            .and_then(|(kind, _, _)| kind.as_ref())
    }

    /// Open `op` in its pane and show the pane: the operation that pane
    /// leaves applied where it was moved and changes something, the other
    /// pane's settled, and one already chosen opened as it is set.
    pub(super) fn open_op(&mut self, op: Op, damage: &mut Region) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        if op.needs_colour() && self.kind().palette().is_some() {
            let refusal = needs_colour(&op);
            self.state(&refusal, damage);
            return Outcome::none();
        }
        let kind = pane_of(op.family());
        self.panes.show(kind);
        self.switch_operation(kind, op.place(), damage)
    }

    /// Apply `op` at once, with nothing to set.
    pub(super) fn apply_now(&mut self, op: Op, damage: &mut Region) -> Outcome {
        match self.apply_op(op, None, Then::Rest, damage) {
            Applying::Now(_) => Outcome::none(),
            Applying::Waiting(outcome) => outcome,
        }
    }

    /// Pane `kind` shows operation `to` of its family: the one it holds left
    /// first — applied as one step where it was moved and changes something,
    /// dropped otherwise — and `to` opened once that has landed; one already
    /// chosen opens as it is set. A refusal keeps the one held as it was set.
    fn switch_operation(&mut self, kind: PaneKind, to: usize, damage: &mut Region) -> Outcome {
        let mut outcome = if self.operation_pane(kind).op().place() == to {
            self.open_chosen(kind, to, damage)
        } else {
            match self.leave_operation(kind, Then::Choose(kind, to), damage) {
                Applying::Now(then) => self.carry_on(then, damage),
                Applying::Waiting(outcome) => self.after_operation(kind, outcome, damage),
            }
        };
        outcome.relayout = Relayout::Whole;
        outcome
    }

    /// Open operation `to` of pane `kind`'s family, the one it held left
    /// already, and lay the pane out anew.
    pub(super) fn open_chosen(
        &mut self,
        kind: PaneKind,
        to: usize,
        damage: &mut Region,
    ) -> Outcome {
        self.operation_mut(kind).choose(to);
        damage.add(self.layout.operation(kind));
        let mut outcome = self.after_operation(kind, Outcome::none(), damage);
        outcome.relayout = Relayout::Whole;
        outcome
    }

    /// Leave pane `kind`'s operation, then carry out `then`: one moved that
    /// changes something is applied — now, or once a worker has completed it
    /// — its pane going back to its starting settings only once it has, and
    /// any other dropped.
    fn leave_operation(&mut self, kind: PaneKind, then: Then, damage: &mut Region) -> Applying {
        if let Some(op) = self.operation_pane(kind).applying() {
            return self.apply_op(op, Some(kind), then, damage);
        }
        self.operation_mut(kind).close();
        damage.add(self.layout.operation(kind));
        Applying::Now(then)
    }

    /// Once pane `kind` has taken an input: where both panes are open, the
    /// other's operation left — applied as one step where it was moved and
    /// changes something, its settings kept until that has landed — so one
    /// preview runs across both, and where that is refused this pane's input
    /// is turned down; the bands retitled; and what the picture shows brought
    /// up to date. One pane at most is open before an input, so one at most
    /// has an operation to apply.
    fn after_operation(
        &mut self,
        kind: PaneKind,
        mut outcome: Outcome,
        damage: &mut Region,
    ) -> Outcome {
        let other = other_of(kind);
        if self.operation_pane(kind).is_open() && self.operation_pane(other).is_open() {
            damage.add(self.layout.operation(other));
            match self.leave_operation(other, Then::Rest, damage) {
                Applying::Now(_) => {}
                Applying::Waiting(waiting) if waiting.request.is_some() => outcome = waiting,
                Applying::Waiting(_) => {
                    self.operation_mut(kind).close();
                    damage.add(self.layout.operation(kind));
                }
            }
        }
        self.retitle_operations(damage);
        self.refresh_previews(damage);
        outcome
    }

    /// Name on each pane's band its operation while it is open, else the
    /// pane.
    fn retitle_operations(&mut self, damage: &mut Region) {
        for kind in PaneKind::OPERATIONS {
            let pane = self.operation_pane(kind);
            let title = if pane.is_open() {
                pane.op().label()
            } else {
                pane.title()
            };
            let header = &mut self.headers[kind.index()];
            if header.title() != title {
                header.set_title(title);
                if let Some(slot) = self.layout.pane(kind) {
                    damage.add(slot.header);
                }
            }
        }
    }

    /// Withhold the panes while a worker has the picture or there is no
    /// picture, and the Effects pane from a palette picture, whose pixels are
    /// one entry each.
    pub(super) fn sync_operations(&mut self) {
        let idle = self.pending.is_none() && self.document.picture().is_some();
        let colour = self.kind().palette().is_none();
        for kind in PaneKind::OPERATIONS {
            let withheld = !idle || (kind == PaneKind::Effects && !colour);
            self.operation_mut(kind).set_withheld(withheld);
        }
    }

    /// Bring what the picture shows of the open operation into line with its
    /// settings and the layer, and tell the panes whether a histogram of the
    /// layer as it stands is at hand. A palette picture's palette is mapped
    /// here. A colour picture's overlay is kept while it shows the operation
    /// over the layer as it stands; a sample of the settings starts the next
    /// from it, still showing the last preview until its own tiles land, and
    /// a move of the picture starts afresh — either way what was asked for the
    /// old one is abandoned. An operation the picture can no longer take is
    /// closed first. Nothing moves while a worker has the picture; the tiles
    /// are asked for by [`wanted_preview`](Self::wanted_preview).
    pub(super) fn refresh_previews(&mut self, damage: &mut Region) {
        self.sync_operations();
        if self.pending.is_some() {
            return;
        }
        self.close_untaken(damage);
        let basis = self.basis();
        self.sync_histogram_ready(basis, damage);
        let open = self.adjusting().filter(|op| !op.is_identity());
        if self.kind().palette().is_some() {
            if self.looks.overlay.take().is_some() {
                self.disown_previews();
            }
            self.map_palette(open.map(|op| (op, basis)), damage);
            return;
        }
        self.looks.kind = None;
        let held = self.looks.overlay.take();
        self.looks.overlay = match (held, open) {
            (Some(overlay), Some(op)) if overlay.shows(basis, &op) => Some(overlay),
            (Some(overlay), Some(op)) if overlay.basis() == basis => {
                self.disown_previews();
                Some(overlay.followed_by(op))
            }
            (held, open) => {
                if held.is_some() {
                    self.disown_previews();
                    damage.add(self.layout.canvas());
                }
                open.filter(|&op| self.looks.dropped != Some((op, basis)))
                    .and_then(|op| self.new_overlay(op, basis, damage))
            }
        };
    }

    /// Close an operation the picture can no longer take — an effect left
    /// open as an undo past a conversion makes it a palette picture — saying
    /// so, so nothing that settles it waits on a step that cannot be made.
    fn close_untaken(&mut self, damage: &mut Region) {
        if self.kind().palette().is_none() {
            return;
        }
        let Some(kind) = self
            .open_operation()
            .filter(|&kind| self.operation_pane(kind).op().needs_colour())
        else {
            return;
        };
        let closed = alloc::format!(
            "{} was closed: it needs a colour picture",
            self.operation_pane(kind).op().label()
        );
        self.operation_mut(kind).close();
        damage.add(self.layout.operation(kind));
        self.retitle_operations(damage);
        self.state(&closed, damage);
    }

    /// A fresh overlay of `op` over the layer as it stands at `basis`, nothing
    /// yet worked out; `None`, saying so, where its room cannot be had.
    fn new_overlay(&mut self, op: Op, basis: Basis, damage: &mut Region) -> Option<Overlay<Basis>> {
        let canvas = self.document.picture()?.canvas();
        let made = Overlay::new(basis, op, canvas, self.selection.as_ref());
        if made.is_err() {
            self.state(NO_ROOM_TO_PREVIEW, damage);
        }
        made.ok()
    }

    /// Map a palette picture's palette as `wanted`'s operation maps colours,
    /// over the picture as it stands at its basis, where that is not what was
    /// mapped already: a mapping refused its room is kept as refused, said
    /// once and not tried again until the operation or the picture moves.
    fn map_palette(&mut self, wanted: Option<(Op, Basis)>, damage: &mut Region) {
        let held = self.looks.kind.as_ref().map(|(_, op, at)| (*op, *at));
        if held == wanted {
            return;
        }
        self.looks.kind = wanted.map(|(op, at)| (mapped_kind(self.kind(), &op), op, at));
        if self
            .looks
            .kind
            .as_ref()
            .is_some_and(|(kind, _, _)| kind.is_none())
        {
            self.state(NO_ROOM_TO_PREVIEW, damage);
        }
        damage.add(self.layout.canvas());
    }

    /// Tell the panes whether a histogram of the layer as it stands at
    /// `basis` is at hand, which Auto reads.
    fn sync_histogram_ready(&mut self, basis: Basis, damage: &mut Region) {
        let ready = self
            .looks
            .histogram
            .as_ref()
            .is_some_and(|(_, at)| *at == basis);
        for kind in PaneKind::OPERATIONS {
            if self.operation_mut(kind).set_histogram_ready(ready) {
                damage.add(self.layout.operation(kind));
            }
        }
    }

    /// The picture's pixels the canvas shows, a pixel to spare about them.
    fn visible(&self) -> Bounds {
        let area = self.layout.canvas();
        let size = self.picture_size();
        let pixel = |point: Point| {
            let (x, y) = self.viewport.to_picture(point, size, area);
            (x.div_euclid(FX), y.div_euclid(FX))
        };
        let (x0, y0) = pixel(Point::new(area.left(), area.top()));
        let (x1, y1) = pixel(Point::new(area.right(), area.bottom()));
        Bounds {
            x0: x0 - 1,
            y0: y0 - 1,
            x1: x1 + 2,
            y1: y1 + 2,
        }
    }

    /// The overlay's tiles still to work out, those in view first, as preview
    /// work: none while a worker has the picture, or while the picture does
    /// not show the operation.
    pub(super) fn wanted_tiles(&self) -> Option<(Asking, PreviewWork)> {
        let op = self.shown_op().filter(|_| self.pending.is_none())?;
        let overlay = self
            .looks
            .overlay
            .as_ref()
            .filter(|overlay| !overlay.is_complete() && overlay.shows(self.basis(), &op))?;
        let tiles = overlay.next_batch(self.visible()).ok()?;
        let source = self.document.picture()?.canvas().try_clone().ok()?;
        let clip = self.selection.clone();
        let work = PreviewWork::Tiles {
            source,
            op,
            clip,
            tiles,
        };
        Some((Asking::Tiles, work))
    }

    /// A histogram of the layer as it stands, as preview work, where a pane
    /// laid out reads one and the one held is of the layer before it moved.
    pub(super) fn wanted_histogram(&self) -> Option<(Asking, PreviewWork)> {
        if self.pending.is_some() {
            return None;
        }
        let reads = PaneKind::OPERATIONS.into_iter().any(|kind| {
            !self.layout.operation(kind).is_empty() && self.operation_pane(kind).reads_histogram()
        });
        let basis = self.basis();
        let held = self
            .looks
            .histogram
            .as_ref()
            .is_some_and(|(_, at)| *at == basis);
        if !reads || held {
            return None;
        }
        let canvas = self.document.picture()?.canvas().try_clone().ok()?;
        let clip = self.selection.clone();
        Some((
            Asking::Histogram(basis),
            PreviewWork::Histogram { canvas, clip },
        ))
    }

    /// Tiles the preview worker worked out for the overlay: taken in where it
    /// is still of the layer as it stands, and repainted where the picture
    /// shows them.
    pub(super) fn land_tiles(&mut self, tiles: Vec<(usize, Arc<Tile>)>, damage: &mut Region) {
        let basis = self.basis();
        let landed = self.tiles_bounds(&tiles);
        let Some(overlay) = self
            .looks
            .overlay
            .as_mut()
            .filter(|overlay| overlay.basis() == basis)
        else {
            return;
        };
        let op = *overlay.op();
        overlay.land(tiles);
        if self.shown_op() == Some(op) {
            self.damage_picture(landed, damage);
        }
    }

    /// A histogram of the layer as it stood at `basis`: a reading of the
    /// picture whichever pane reads it, kept while it is of the layer as it
    /// stands.
    pub(super) fn land_histogram(
        &mut self,
        histogram: Histogram,
        basis: Basis,
        damage: &mut Region,
    ) {
        if basis != self.basis() {
            return;
        }
        self.looks.histogram = Some((histogram, basis));
        self.sync_histogram_ready(basis, damage);
        for kind in PaneKind::OPERATIONS {
            if self.operation_pane(kind).reads_histogram() {
                damage.add(self.layout.operation(kind));
            }
        }
    }

    /// Let the overlay go to memory pressure, and what the preview worker is
    /// doing for the window with it: none is made again until the settings or
    /// the picture move.
    pub(super) fn drop_overlay(&mut self, damage: &mut Region) {
        let Some(overlay) = self.looks.overlay.take() else {
            return;
        };
        self.looks.dropped = Some((*overlay.op(), overlay.basis()));
        self.disown_previews();
        damage.add(self.layout.canvas());
    }

    /// The pixels `tiles` of the picture showing cover.
    pub(super) fn tiles_bounds(&self, tiles: &[(usize, Arc<Tile>)]) -> Option<Bounds> {
        let canvas = self.document.picture()?.canvas();
        tiles
            .iter()
            .map(|(index, _)| canvas.tile_rect(*index).bounds())
            .reduce(|a, b| a.union(&b))
    }

    /// Keep of `tiles` those that differ from layer `layer`'s own: an
    /// operation answers a tile it leaves as it was as the very tile, and
    /// that is no change to record.
    pub(super) fn keep_changed(&self, layer: usize, tiles: &mut Vec<(usize, Arc<Tile>)>) {
        let Some(canvas) = self
            .document
            .picture()
            .and_then(|picture| picture.layers().get(layer))
            .map(|held| &held.canvas)
        else {
            return;
        };
        tiles.retain(|(index, tile)| {
            *index >= canvas.tile_count() || !Arc::ptr_eq(tile, canvas.tile(*index))
        });
    }

    /// Apply `op` to the layer painted on as one step, then carry out `then`;
    /// the pane `closes` names goes back to its starting settings once it is
    /// applied, and keeps them where that is refused. A palette picture's
    /// palette is mapped, and an effect it cannot take refused; a colour
    /// picture takes the overlay where it shows `op` whole, and what an
    /// incomplete one lacks is worked out on the ordered queue, `then` waiting
    /// for it.
    pub(super) fn apply_op(
        &mut self,
        op: Op,
        closes: Option<PaneKind>,
        then: Then,
        damage: &mut Region,
    ) -> Applying {
        if !self.editable(damage) {
            return Applying::Waiting(Outcome::none());
        }
        let applied = if op.is_identity() {
            true
        } else if op.needs_colour() && self.kind().palette().is_some() {
            let refusal = needs_colour(&op);
            self.state(&refusal, damage);
            return Applying::Waiting(Outcome::none());
        } else if let Some(palette) = self.kind().palette() {
            let Some(mapped) = op.mapped_palette(palette) else {
                self.state(NO_ROOM_TO_APPLY, damage);
                return Applying::Waiting(Outcome::none());
            };
            if !self.change_palette(mapped, damage) {
                return Applying::Waiting(Outcome::none());
            }
            true
        } else {
            let Ok(adopted) = self.adopt_overlay(&op, damage) else {
                self.state(NO_ROOM_TO_APPLY, damage);
                return Applying::Waiting(Outcome::none());
            };
            adopted
        };
        if !applied {
            return self.complete_on_queue(op, closes, then, damage);
        }
        self.operation_applied(closes, damage);
        Applying::Now(then)
    }

    /// Land the overlay as one step where it shows `op` whole over the layer
    /// as it stands, answering whether it did.
    fn adopt_overlay(&mut self, op: &Op, damage: &mut Region) -> Result<bool, OutOfMemory> {
        let basis = self.basis();
        let Some(overlay) = self
            .looks
            .overlay
            .as_ref()
            .filter(|overlay| overlay.is_complete() && overlay.shows(basis, op))
        else {
            return Ok(false);
        };
        let mut tiles = overlay.done()?;
        let layer = self.active_layer();
        self.keep_changed(layer, &mut tiles);
        if tiles.is_empty() {
            return Ok(true);
        }
        let bounds = self.tiles_bounds(&tiles);
        let adopted = self.document.adopt_tiles(layer, tiles)?;
        if adopted {
            self.damage_picture(bounds, damage);
        }
        Ok(adopted)
    }

    /// Work out on the ordered queue what the overlay lacks of `op`, landing
    /// the whole as one step and `then` once it has; the preview shows until
    /// it lands, and the preview worker is left free.
    fn complete_on_queue(
        &mut self,
        op: Op,
        closes: Option<PaneKind>,
        then: Then,
        damage: &mut Region,
    ) -> Applying {
        let basis = self.basis();
        let done = self
            .looks
            .overlay
            .as_ref()
            .filter(|overlay| overlay.shows(basis, &op))
            .map_or_else(|| Ok(Vec::new()), Overlay::done);
        let canvas = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone());
        let (Some(Ok(canvas)), Ok(done)) = (canvas, done) else {
            self.state(NO_ROOM_TO_APPLY, damage);
            return Applying::Waiting(Outcome::none());
        };
        self.disown_previews();
        self.looks.applying = Some(op);
        let work = Compute::Op {
            canvas,
            op,
            clip: self.selection.clone(),
            done,
        };
        let lands = Lands::Tiles(Settles::Adjusted { closes, then });
        Applying::Waiting(self.begin_work(work, lands, APPLYING, damage))
    }

    /// Drop pane `kind`'s operation without applying it: its pane was
    /// hidden.
    pub(super) fn close_operation(&mut self, kind: PaneKind, damage: &mut Region) {
        self.operation_mut(kind).close();
        self.retitle_operations(damage);
        self.refresh_previews(damage);
    }

    /// A worker's job for the picture is over: no operation is being applied.
    pub(super) fn end_applying(&mut self) {
        self.looks.applying = None;
    }

    /// An operation landed, at once or from a worker: the pane `closes` names
    /// back at its starting settings.
    pub(super) fn operation_applied(&mut self, closes: Option<PaneKind>, damage: &mut Region) {
        if let Some(kind) = closes {
            self.operation_mut(kind).applied();
            damage.add(self.layout.operation(kind));
        }
        self.retitle_operations(damage);
    }

    /// Applying pane `kind`'s operation was refused once a worker had it: the
    /// operation stays open as it was set, and the other pane, opened while it
    /// was worked out, goes back to its starting settings, so one is open.
    pub(super) fn operation_refused(&mut self, kind: PaneKind, damage: &mut Region) {
        let other = other_of(kind);
        if self.operation_pane(other).is_open() {
            self.operation_mut(other).close();
            damage.add(self.layout.operation(other));
        }
        self.retitle_operations(damage);
    }

    /// Apply the open operation before an action that changes or reads the
    /// layer, then carry out `then`: one moved that changes something is
    /// applied, now or once a worker has completed it, its pane going back to
    /// its starting settings only once it has, and any other dropped.
    pub(super) fn apply_adjustment(&mut self, then: Then, damage: &mut Region) -> Applying {
        let Some(kind) = self.open_operation() else {
            return Applying::Now(then);
        };
        let applying = self.leave_operation(kind, then, damage);
        self.retitle_operations(damage);
        self.refresh_previews(damage);
        applying
    }

    /// Whether `action` leaves the open operation open: the view, the inks,
    /// the panes, choosing a tool, undo and redo, the selection, the
    /// document's name, how the layers show and in what order, and opening an
    /// operation, which settles the open one itself. Anything else that
    /// changes or reads the layer — painting on another among them — applies
    /// it first.
    pub(super) fn leaves_adjustment(&self, action: Action) -> bool {
        match action {
            Action::ZoomIn
            | Action::ZoomOut
            | Action::Fit
            | Action::Actual
            | Action::PixelGrid
            | Action::Grid
            | Action::Zoom(_)
            | Action::EditPrimary
            | Action::EditSecondary
            | Action::SwapColours
            | Action::Pane(_)
            | Action::ResetPanes
            | Action::FloatPalette
            | Action::Tool(_)
            | Action::Undo
            | Action::Redo
            | Action::SelectAll
            | Action::Deselect
            | Action::Rename
            | Action::ShowLayer
            | Action::RaiseLayer
            | Action::LowerLayer
            | Action::Adjust(_) => true,
            Action::PutDown | Action::Delete => self.draft.is_some(),
            _ => false,
        }
    }

    /// Apply pressed in pane `kind`: the other pane's operation, where it is
    /// open, left first — applied as one step where it is to be — and this
    /// one's applied as one step once that has landed, at once from a whole
    /// overlay, else once a worker has completed it.
    pub(super) fn apply_pressed(&mut self, kind: PaneKind, damage: &mut Region) -> Outcome {
        let other = other_of(kind);
        if self.operation_pane(other).is_open() {
            return match self.leave_operation(other, Then::Apply(kind), damage) {
                Applying::Now(then) => self.carry_on(then, damage),
                Applying::Waiting(outcome) => self.after_operation(kind, outcome, damage),
            };
        }
        let op = self.operation_pane(kind).op();
        let outcome = match self.apply_op(op, Some(kind), Then::Rest, damage) {
            Applying::Now(_) => Outcome::none(),
            Applying::Waiting(outcome) => outcome,
        };
        self.after_operation(kind, outcome, damage)
    }

    /// Auto in pane `kind`, reading the histogram of the layer as it stands.
    fn auto(&mut self, kind: PaneKind, damage: &mut Region) -> Outcome {
        let basis = self.basis();
        let index = kind.operation().unwrap_or(0);
        let moved = match &self.looks.histogram {
            Some((histogram, at)) if *at == basis => self.operations[index].auto(histogram),
            _ => false,
        };
        if !moved {
            return Outcome::none();
        }
        damage.add(self.layout.operation(kind));
        self.after_operation(kind, Outcome::none(), damage)
    }

    /// What pane `kind`'s answer comes to for the window.
    fn adjusted(&mut self, kind: PaneKind, answer: OpOutcome, damage: &mut Region) -> Outcome {
        match answer {
            OpOutcome::Ignored | OpOutcome::Taken | OpOutcome::Left { .. } => Outcome::none(),
            OpOutcome::Changed { .. } => {
                self.end_gesture(damage);
                self.after_operation(kind, Outcome::none(), damage)
            }
            OpOutcome::Previewed => {
                self.end_gesture(damage);
                damage.add(self.layout.canvas());
                self.after_operation(kind, Outcome::none(), damage)
            }
            OpOutcome::Picking => {
                damage.add(self.layout.canvas());
                Outcome::none()
            }
            OpOutcome::Auto => self.auto(kind, damage),
            OpOutcome::Apply => self.apply_pressed(kind, damage),
            OpOutcome::Switched { to } => self.switch_operation(kind, to, damage),
        }
    }

    /// The pointer on the Adjustments and the Effects panes, mapped into a
    /// body its dock scrolls: `None` where the event is none of theirs. Each
    /// sees every event, so a hover leaves the one it left; a list one
    /// opened, or a press held on one, keeps the pointer for that one alone.
    pub(super) fn adjustment_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let look = (Faces::of(theme, scale), scale, theme);
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        let owner = PaneKind::OPERATIONS.into_iter().find(|&kind| {
            let pane = self.operation_pane(kind);
            pane.listing() || pane.holding()
        });
        let mut answered = None;
        let mut pressed_on = false;
        for kind in PaneKind::OPERATIONS {
            let bounds = self.layout.operation_settings(kind);
            let held = owner == Some(kind);
            if owner.is_some_and(|owner| owner != kind) || (bounds.is_empty() && !held) {
                continue;
            }
            let view = self.held_view(kind, held);
            let room = self.list_room(kind);
            let mut drew = damage::sink();
            let answer = self.operation_mut(kind).on_pointer(
                &view.event_in_layout(event),
                (bounds, room),
                look,
                &mut drew,
            );
            view.report(&drew, damage);
            pressed_on |= pressed && self.layout.operation(kind).contains(self.pointer);
            if answered.is_none() && answer != OpOutcome::Ignored {
                answered = Some((kind, answer));
            }
        }
        match answered {
            Some((kind, answer)) => Some(self.adjusted(kind, answer, damage)),
            None => pressed_on.then(Outcome::none),
        }
    }

    /// A key while the Adjustments or the Effects pane has the keyboard, the
    /// part it lands on scrolled into view: `None` where it is none of the
    /// pane's.
    pub(super) fn adjustment_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let kind = PaneKind::OPERATIONS
            .into_iter()
            .find(|&kind| self.operation_pane(kind).has_focus())?;
        let look = (Faces::of(theme, scale), scale, theme);
        let bounds = self.layout.operation_settings(kind);
        let view = self.held_view(kind, self.operation_pane(kind).listing());
        let room = self.list_room(kind);
        let mut drew = damage::sink();
        let answer = self
            .operation_mut(kind)
            .on_key(stroke, (bounds, room), look, &mut drew);
        view.report(&drew, damage);
        match answer {
            OpOutcome::Ignored => None,
            OpOutcome::Left { forward } => {
                self.walk_keyboard(Keyboard::Operation(kind), forward, scale, theme, damage);
                Some(Outcome::none())
            }
            answer => {
                self.reveal_operation(kind, look, damage);
                Some(self.adjusted(kind, answer, damage))
            }
        }
    }

    /// Scroll pane `kind`'s body, where its dock scrolls it, so the part
    /// holding the keyboard shows.
    fn reveal_operation(
        &mut self,
        kind: PaneKind,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) {
        let bounds = self.layout.operation_settings(kind);
        if let Some(rect) = self.operation_pane(kind).focus_rect(bounds, look) {
            self.reveal_body(kind, rect, damage);
        }
    }

    /// Settle the typing of the Adjustments and the Effects panes but `keep`,
    /// and take the keyboard from them, as a press elsewhere or a menu does.
    pub(super) fn release_adjustment(
        &mut self,
        keep: Option<PaneKind>,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let look = (Faces::of(theme, scale), scale, theme);
        for kind in PaneKind::OPERATIONS {
            if Some(kind) == keep {
                continue;
            }
            let view = self.held_view(kind, false);
            let mut drew = damage::sink();
            let settings = self.layout.operation_settings(kind);
            let answer = self.operation_mut(kind).blur(settings, look, &mut drew);
            view.report(&drew, damage);
            if matches!(answer, OpOutcome::Changed { .. }) {
                self.retitle_operations(damage);
                self.refresh_previews(damage);
            }
        }
    }

    /// Give pane `kind`, the Adjustments or the Effects pane, the keyboard at
    /// its first stop, or its last, scrolled into view.
    pub(super) fn enter_adjustment(
        &mut self,
        kind: PaneKind,
        forward: bool,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let bounds = self.layout.operation_settings(kind);
        if bounds.is_empty() {
            return false;
        }
        let mut drew = damage::sink();
        let entered = self
            .operation_mut(kind)
            .enter_focus(forward, bounds, &mut drew);
        self.held_view(kind, false).report(&drew, damage);
        if entered {
            let look = (Faces::of(theme, scale), scale, theme);
            self.reveal_operation(kind, look, damage);
        }
        entered
    }

    /// Put an eyedropper that is up down, answering whether one was.
    pub(super) fn put_down_pick(&mut self, damage: &mut Region) -> bool {
        let Some(kind) = self.picking_operation() else {
            return false;
        };
        if self.operation_mut(kind).put_down_pick() {
            damage.add(self.layout.operation(kind));
        }
        true
    }

    /// A press on the picture while pane `kind`'s eyedropper is up: take the
    /// colour of the layer painted on there, as it is beneath the preview.
    pub(super) fn pick_for_adjustment(
        &mut self,
        kind: PaneKind,
        at: Fx,
        damage: &mut Region,
    ) -> Outcome {
        let (x, y) = at.pixel();
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return Outcome::none();
        };
        let Some(colour) = self.document.picture().and_then(|picture| {
            picture
                .canvas()
                .sample(x, y)
                .map(|sample| picture.kind().colour(sample))
        }) else {
            return Outcome::none();
        };
        let [red, green, blue, _] = colour;
        damage.add(self.layout.operation(kind));
        if self.operation_mut(kind).picked(Rgb::new(red, green, blue)) {
            return self.after_operation(kind, Outcome::none(), damage);
        }
        Outcome::none()
    }
}

/// The pane `family`'s operations are set in.
const fn pane_of(family: Family) -> PaneKind {
    match family {
        Family::Adjustments => PaneKind::Adjustments,
        Family::Effects => PaneKind::Effects,
    }
}

/// The other of the two operation panes.
const fn other_of(kind: PaneKind) -> PaneKind {
    match kind {
        PaneKind::Effects => PaneKind::Adjustments,
        _ => PaneKind::Effects,
    }
}

/// Why a palette picture, whose pixels are one entry each, takes no `op`.
fn needs_colour(op: &Op) -> String {
    alloc::format!(
        "{} needs a colour picture: convert it to millions of colours first",
        op.label()
    )
}

/// `kind`, a palette picture's, with its palette mapped as `op` maps colours;
/// `None` for an effect, or where the room cannot be had.
fn mapped_kind(kind: &Kind, op: &Op) -> Option<Kind> {
    let palette = op.mapped_palette(kind.palette()?)?;
    match kind {
        Kind::Indexed { depth, masked, .. } => Some(Kind::Indexed {
            depth: *depth,
            palette,
            masked: *masked,
        }),
        Kind::Rgba => None,
    }
}

/// What applying an open operation came to.
#[allow(
    clippy::large_enum_variant,
    reason = "handed straight back to the caller and never stored; boxing would allocate on every apply"
)]
pub(super) enum Applying {
    /// Applied now, or nothing was open: what follows is carried out now.
    Now(Then),
    /// A worker applies it and what follows waits for it to land, or it
    /// could not be applied; this is what the window does meanwhile.
    Waiting(Outcome),
}
