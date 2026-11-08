//! What the window does with the picture's layers: adding, removing and
//! ordering them, which one is painted on, how each shows, and laying them
//! together on a worker; and the Layers pane — its rows picking, showing,
//! renaming and moving their layers, the opacity of the layer painted on
//! changed live over the composite beneath it, and the rows' thumbnails kept
//! by revision, the two worked out on the preview worker.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_colour::Fraction;
use tairix_controls::Keystroke;
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::input::Keyboard;
use super::operations::Applying;
use super::previews::Asking;
use super::{Action, Compute, Lands, Outcome, Then, View};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::{copy_name, new_layer_name, Document, Layer, LayerRefusal, Shown};
use crate::layers_pane::{layer_at, thumb_side, LayersOutcome, LayersPane};
use crate::layout::Faces;
use crate::pane::{PaneKind, Side};
use crate::preview::{PreviewWork, Thumbnails};
use crate::shape::Bounds;

/// Why a change to the layers could not be had for want of memory.
const NO_ROOM: &str = "There is not enough memory to change the layers";

/// Most thumbnails one job draws, so the rows in view land first.
const THUMBS_PER_JOB: usize = 8;

/// The state thumbnails were looked for at: the document's generation, the
/// entry showing, the side they are drawn at and the rows in view.
pub(super) type Looked = (u64, usize, u32, Range<usize>);

/// A layer's thumbnail as it landed: the side it was drawn at, and its
/// picture where one could be drawn.
#[derive(Debug)]
struct Thumb {
    side: u32,
    surface: Option<Surface>,
}

/// The layers beneath the one whose opacity is changing, laid together, and
/// the entry, layer and generation they were laid from.
#[derive(Debug)]
struct Under {
    basis: (usize, usize, u64),
    canvas: Canvas,
}

/// The opacity of the layer painted on being changed live: how the layer
/// showed before, which settling records the change against as one step.
#[derive(Debug)]
struct OpacityEdit {
    layer: usize,
    before: Shown,
    /// Whether the composite beneath it is wanted at all: never for one
    /// settled at its first sample, nor once memory pressure let it go.
    wants_under: bool,
}

/// What the window keeps for its Layers pane beyond the pane itself.
#[derive(Debug, Default)]
pub(crate) struct LayerLooks {
    /// Each layer's thumbnail, by the revision it was drawn from.
    thumbs: BTreeMap<u64, Thumb>,
    /// What was last looked at wanting nothing, or asked of and refused:
    /// nothing is asked again until it moves.
    seen: Option<Looked>,
    edit: Option<OpacityEdit>,
    under: Option<Under>,
}

/// The showing picture's layers and the one painted on; none for a sprite
/// kept as its bytes.
fn slice_of(document: &Document) -> (&[Layer], usize) {
    document
        .picture()
        .map_or((&[][..], 0), |picture| (picture.layers(), picture.active()))
}

impl View {
    /// Add a clear layer over the one painted on, and paint on it.
    pub(super) fn new_layer(&mut self, damage: &mut Region) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        let (width, height) = picture.size();
        let made = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))
            .ok()
            .zip(new_layer_name(picture.layers()).ok());
        let Some((canvas, name)) = made else {
            self.state(NO_ROOM, damage);
            return Outcome::none();
        };
        let at = picture.active() + 1;
        self.change_layers(
            |document| document.insert_layer(at, Layer::new(canvas, name)),
            damage,
        )
    }

    /// Add a copy of the layer painted on over it, and paint on the copy.
    pub(super) fn duplicate_layer(&mut self, damage: &mut Region) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        let active = picture.active();
        let copied = picture.layers().get(active).and_then(|layer| {
            let mut copy = layer.try_clone().ok()?;
            copy.name = copy_name(&layer.name).ok()?;
            Some(copy)
        });
        let Some(copy) = copied else {
            self.state(NO_ROOM, damage);
            return Outcome::none();
        };
        self.change_layers(|document| document.insert_layer(active + 1, copy), damage)
    }

    /// Take the layer painted on away.
    pub(super) fn delete_layer(&mut self, damage: &mut Region) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        let active = self.active_layer();
        self.change_layers(|document| document.remove_layer(active), damage)
    }

    /// Move the layer painted on one place up the stack, or down it.
    pub(super) fn move_layer(&mut self, up: bool, damage: &mut Region) -> Outcome {
        match self.beside(up) {
            Some(to) => self.reorder_layer(self.active_layer(), to, damage),
            None => Outcome::none(),
        }
    }

    /// Move layer `from` to `to` in the stack as one step, painting on it
    /// there.
    fn reorder_layer(&mut self, from: usize, to: usize, damage: &mut Region) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        self.change_layers(|document| document.move_layer(from, to), damage)
    }

    /// Paint on the layer above the one painted on, or the one below.
    pub(super) fn step_layer(&mut self, up: bool, damage: &mut Region) -> Outcome {
        if !self.idle(damage) {
            return Outcome::none();
        }
        if let Some(to) = self.beside(up) {
            self.paint_on(to, damage);
        }
        Outcome::none()
    }

    /// The layer next to the one painted on, up the stack or down it.
    fn beside(&self, up: bool) -> Option<usize> {
        let picture = self.document.picture()?;
        let active = picture.active();
        if up {
            Some(active + 1).filter(|&to| to < picture.layers().len())
        } else {
            active.checked_sub(1)
        }
    }

    /// Paint on the layer `text` names, or numbers from one at the bottom.
    pub(super) fn go_to_layer(&mut self, text: &str, damage: &mut Region) -> Outcome {
        if !self.idle(damage) {
            return Outcome::none();
        }
        let text = text.trim();
        let found = self.document.picture().and_then(|picture| {
            let layers = picture.layers();
            layers
                .iter()
                .position(|layer| layer.name == text)
                .or_else(|| {
                    text.parse::<usize>()
                        .ok()
                        .and_then(|number| number.checked_sub(1))
                        .filter(|&index| index < layers.len())
                })
        });
        match found {
            Some(index) => self.paint_on(index, damage),
            None => self.state("There is no layer of that name or number", damage),
        }
        Outcome::none()
    }

    /// Paint on layer `index`: where painting lands changes, the picture does
    /// not. The Layers pane marks its row, scrolled into view, and shows its
    /// opacity.
    fn paint_on(&mut self, index: usize, damage: &mut Region) {
        let was = self.active_layer();
        if !self.document.select_layer(index) {
            return;
        }
        damage.add(self.layout.status());
        let placed = self.layout.layers();
        if self.layers.reveal(index, placed) {
            damage.add(placed.rows);
        } else {
            damage.add(self.layers.row_rect(was, placed));
            damage.add(self.layers.row_rect(index, placed));
        }
        self.sync_layers(damage);
    }

    /// Show the layer painted on, or hide it.
    pub(super) fn toggle_layer(&mut self, damage: &mut Region) -> Outcome {
        self.toggle_shown(self.active_layer(), damage)
    }

    /// Show layer `layer`, or hide it, as one step, painting staying on the
    /// layer it was on; an opacity still changing lands first, so the step
    /// records how the layer showed once that had settled.
    fn toggle_shown(&mut self, layer: usize, damage: &mut Region) -> Outcome {
        self.settle_opacity(damage);
        if !self.editable(damage) {
            return Outcome::none();
        }
        let shown = self
            .document
            .picture()
            .and_then(|picture| picture.layers().get(layer))
            .map(Shown::of);
        match shown {
            Some(Ok(mut shown)) => {
                shown.visible = !shown.visible;
                self.reshow(layer, shown, damage)
            }
            Some(Err(_)) => {
                self.state(NO_ROOM, damage);
                Outcome::none()
            }
            None => Outcome::none(),
        }
    }

    /// Show layer `layer` as `shown` says, as one step, painting staying on
    /// the layer it was on.
    fn reshow(&mut self, layer: usize, shown: Shown, damage: &mut Region) -> Outcome {
        let active = self.active_layer();
        self.change_layers(
            |document| {
                document.show_layer(layer, shown)?;
                document.select_layer(active);
                Ok(())
            },
            damage,
        )
    }

    /// Lay the layer painted on over the one beneath it, the two becoming
    /// one, on a worker. Both must show: a merge keeps the look, so the pixels
    /// of a layer hidden or wholly faint would have nowhere to go.
    pub(super) fn merge_down(&mut self, damage: &mut Region) -> Outcome {
        let active = self.active_layer();
        let Some(lowest) = active.checked_sub(1) else {
            self.state("There is no layer beneath this one", damage);
            return Outcome::none();
        };
        if !self
            .document
            .picture()
            .is_some_and(|picture| picture.shows(lowest..active + 1))
        {
            self.state("A hidden or wholly faint layer is not merged", damage);
            return Outcome::none();
        }
        self.compose(lowest..active + 1, "merge the layers", damage)
    }

    /// Lay every layer together as one, on a worker.
    pub(super) fn flatten(&mut self, damage: &mut Region) -> Outcome {
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        if picture.single() {
            self.state("The picture is one layer, shown wholly, already", damage);
            return Outcome::none();
        }
        let count = picture.layers().len();
        self.compose(0..count, "flatten the picture", damage)
    }

    /// Hand the layers in `range` to a worker to be laid together.
    fn compose(
        &mut self,
        range: core::ops::Range<usize>,
        what: &'static str,
        damage: &mut Region,
    ) -> Outcome {
        if !self.editable(damage) {
            return Outcome::none();
        }
        let shared = self.document.picture().and_then(|picture| {
            let laid = picture.layers().get(range.clone())?;
            let mut layers = Vec::new();
            layers.try_reserve_exact(laid.len()).ok()?;
            for layer in laid {
                layers.push(layer.try_clone().ok()?);
            }
            Some(layers)
        });
        let Some(layers) = shared else {
            self.state(
                &alloc::format!("There is not enough memory to {what}"),
                damage,
            );
            return Outcome::none();
        };
        self.begin_work(
            Compute::Compose { layers },
            Lands::Merged(range),
            what,
            damage,
        )
    }

    /// Make `change` to the document's layers, showing what it changed or
    /// saying why it was refused.
    fn change_layers(
        &mut self,
        change: impl FnOnce(&mut Document) -> Result<(), LayerRefusal>,
        damage: &mut Region,
    ) -> Outcome {
        match change(&mut self.document) {
            Ok(()) => self.layers_changed(damage),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), damage);
                Outcome::none()
            }
        }
    }

    /// The layers changed, the picture's size and kind kept: what shows, what
    /// the status band says of them and the Layers pane's rows are drawn
    /// again; a layer come or gone while the pane shows lays the window out
    /// again, with the column or the tool window the pane stands in.
    pub(super) fn layers_changed(&mut self, damage: &mut Region) -> Outcome {
        damage.add(self.layout.canvas());
        damage.add(self.layout.status());
        self.sync_layers(damage);
        let placed = self.layout.layers();
        damage.add(placed.rows);
        if placed.bounds.is_empty() || placed.count == slice_of(&self.document).0.len() {
            return Outcome::none();
        }
        let column = Side::BOTH
            .into_iter()
            .map(|side| self.layout.dock_on(side))
            .find(|dock| dock.panes.iter().any(|slot| slot.kind == PaneKind::Layers))
            .map_or_else(
                || self.layout.pane_window(PaneKind::Layers),
                |dock| dock.rect,
            );
        damage.add(column);
        Outcome::reshaped()
    }

    /// The Layers pane, for the painter.
    pub(crate) const fn layers_pane(&self) -> &LayersPane {
        &self.layers
    }

    /// The thumbnail drawn of a layer at `revision`, `side` pixels square,
    /// where one has landed.
    pub(crate) fn layer_thumb(&self, revision: u64, side: u32) -> Option<&Surface> {
        self.layer_looks
            .thumbs
            .get(&revision)
            .filter(|thumb| thumb.side == side)?
            .surface
            .as_ref()
    }

    /// The layers beneath the one whose opacity is changing, laid together,
    /// while they are of the picture as it stands.
    pub(crate) fn composite_under(&self) -> Option<&Canvas> {
        let under = self.layer_looks.under.as_ref()?;
        (under.basis == self.layer_basis()).then_some(&under.canvas)
    }

    /// The entry showing, the layer painted on and the document's generation.
    fn layer_basis(&self) -> (usize, usize, u64) {
        (
            self.document.current(),
            self.active_layer(),
            self.document.generation(),
        )
    }

    /// Show the opacity of the layer painted on in the Layers pane, and offer
    /// there what may be done to the layers now, a rename whose layer has gone
    /// dropped.
    pub(super) fn sync_layers(&mut self, damage: &mut Region) {
        let colour = self
            .document
            .picture()
            .is_some_and(|picture| picture.kind().palette().is_none());
        let idle = self.pending.is_none();
        let (layers, active) = slice_of(&self.document);
        let mergeable = active > 0
            && self
                .document
                .picture()
                .is_some_and(|picture| picture.shows(active - 1..active + 1));
        let renaming = self.layers.renaming();
        if self
            .layers
            .sync((layers, active), (colour, mergeable, idle))
        {
            damage.add(self.layout.layers().panel);
        }
        if self.layers.renaming() != renaming {
            damage.add(self.layout.layers().rows);
        }
    }

    /// The pointer on the Layers pane: `None` where the event is none of its.
    /// A press beside the opacity first settles what was typed there, and one
    /// beside a name being typed settles the name.
    pub(super) fn layers_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let placed = *self.layout.layers();
        if placed.bounds.is_empty() && !self.layers.holding() {
            return None;
        }
        let look = (Faces::of(theme, scale), scale, theme);
        let pressed = matches!(event, InputEvent::PointerPressed { .. });
        if pressed && !placed.panel.contains(self.pointer) {
            let settled = self.layers.blur_panel(&placed, look, damage);
            let _ = self.layers_answered(settled, scale, theme, damage);
        }
        let (layers, active) = slice_of(&self.document);
        match self
            .layers
            .on_pointer(event, &placed, (layers, active), look, damage)
        {
            LayersOutcome::Ignored => {
                (pressed && self.layout.layers_body().contains(self.pointer)).then(Outcome::none)
            }
            outcome => Some(self.layers_answered(outcome, scale, theme, damage)),
        }
    }

    /// A key while the Layers pane has the keyboard: `None` where it is the
    /// window's.
    pub(super) fn layers_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let placed = *self.layout.layers();
        let look = (Faces::of(theme, scale), scale, theme);
        let (layers, active) = slice_of(&self.document);
        match self
            .layers
            .on_key(stroke, &placed, (layers, active), look, damage)
        {
            LayersOutcome::Ignored => self.leave_on(Keyboard::Layers, stroke, scale, theme, damage),
            outcome => Some(self.layers_answered(outcome, scale, theme, damage)),
        }
    }

    /// Settle what the Layers pane holds typed — an opacity, a name — and
    /// take the keyboard from it.
    pub(super) fn release_layers(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        let look = (Faces::of(theme, scale), scale, theme);
        let settled = self.layers.blur(self.layout.layers(), look, damage);
        let _ = self.layers_answered(settled, scale, theme, damage);
    }

    /// Give the Layers pane the keyboard, at its first stop or its last.
    pub(super) fn enter_layers(&mut self, forward: bool, damage: &mut Region) -> bool {
        let placed = *self.layout.layers();
        !placed.bounds.is_empty() && self.layers.enter_focus(forward, &placed, damage)
    }

    /// Scroll the Layers pane's rows by a turn of the wheel over them,
    /// answering whether the turn was theirs.
    pub(super) fn wheel_layers(&mut self, dy: i32, scale: Scale, damage: &mut Region) -> bool {
        let placed = *self.layout.layers();
        if !placed.rows.contains(self.pointer) {
            return false;
        }
        self.layers.wheel(dy, &placed, scale, damage);
        true
    }

    /// What the Layers pane's answer comes to for the window.
    fn layers_answered(
        &mut self,
        outcome: LayersOutcome,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match outcome {
            LayersOutcome::Ignored | LayersOutcome::Taken => Outcome::none(),
            LayersOutcome::Select(layer) => self.work_on(layer, damage),
            LayersOutcome::Show(layer) => self.toggle_shown(layer, damage),
            LayersOutcome::Renamed(revision) => self.rename_layer(revision, damage),
            LayersOutcome::Move { from, to } => self.reorder_layer(from, to, damage),
            LayersOutcome::Opacity { percent, settled } => {
                self.layer_opacity(percent, settled, damage);
                Outcome::none()
            }
            LayersOutcome::New => self.act(Action::NewLayer, damage),
            LayersOutcome::Duplicate => self.act(Action::DuplicateLayer, damage),
            LayersOutcome::Delete => self.act(Action::DeleteLayer, damage),
            LayersOutcome::MergeDown => self.act(Action::MergeDown, damage),
            LayersOutcome::Left { forward } => {
                self.walk_keyboard(Keyboard::Layers, forward, scale, theme, damage);
                Outcome::none()
            }
        }
    }

    /// Paint on layer `layer`, picked in the Layers pane — no step is taken —
    /// an opacity still changing landed, the open operation applied and a
    /// floating selection put down first, as stepping to a layer does, each
    /// coming back here once it has landed.
    pub(super) fn work_on(&mut self, layer: usize, damage: &mut Region) -> Outcome {
        self.end_gesture(damage);
        self.settle_opacity(damage);
        if self.adjusting().is_some() {
            return match self.apply_adjustment(Then::PaintOn(layer), damage) {
                Applying::Now(then) => self.carry_on(then, damage),
                Applying::Waiting(outcome) => outcome,
            };
        }
        if self.held.is_some() {
            return self.put_down_then(Then::PaintOn(layer), damage);
        }
        if self.idle(damage) {
            self.paint_on(layer, damage);
        }
        Outcome::none()
    }

    /// Call the layer at `revision` what the Layers pane's name field holds,
    /// as one step, and close the field, an opacity still changing landed
    /// first: a name left empty keeps the one it had, one whose layer no
    /// longer holds that revision lands on none, and one longer than a layer's
    /// may be is refused, saying so.
    fn rename_layer(&mut self, revision: u64, damage: &mut Region) -> Outcome {
        self.settle_opacity(damage);
        let typed = self.layers.typed_name().map_or("", str::trim);
        let mut name = String::new();
        let room = name.try_reserve_exact(typed.len()).is_ok();
        if room {
            name.push_str(typed);
        }
        let (layers, _) = slice_of(&self.document);
        let found = layer_at(layers, revision).and_then(|layer| {
            let held = layers.get(layer)?;
            Some((layer, held.opacity, held.visible))
        });
        self.layers.end_rename(layers, self.layout.layers(), damage);
        if !room {
            self.state(NO_ROOM, damage);
            return Outcome::none();
        }
        let Some((layer, opacity, visible)) = found else {
            return Outcome::none();
        };
        if name.is_empty() || !self.editable(damage) {
            return Outcome::none();
        }
        let shown = Shown {
            name,
            opacity,
            visible,
        };
        self.reshow(layer, shown, damage)
    }

    /// Land a name being typed in the Layers pane, as a press beside it does:
    /// what every action, a menu entry, a close and the window losing the
    /// keyboard do first.
    pub(super) fn settle_rename(&mut self, damage: &mut Region) {
        if let Some(revision) = self.layers.renaming() {
            let _ = self.rename_layer(revision, damage);
        }
    }

    /// Show the layer painted on at `percent` live, as the Layers pane's
    /// opacity moves: laid over the composite of the layers beneath once the
    /// preview worker has that, and recorded as one step once `settled`.
    pub(crate) fn layer_opacity(&mut self, percent: i32, settled: bool, damage: &mut Region) {
        if self.layer_looks.edit.is_none() && !self.begin_opacity(settled, damage) {
            return;
        }
        let layer = self.layer_looks.edit.as_ref().map_or(0, |edit| edit.layer);
        let opacity = Fraction::from_percent(u32::try_from(percent).unwrap_or(0)).byte();
        if self.document.set_opacity_live(layer, opacity) {
            let (width, height) = self.picture_size();
            self.damage_picture(Some(Bounds::picture(width, height)), damage);
            damage.add(self.layers.row_rect(layer, self.layout.layers()));
        }
        if settled {
            self.settle_opacity(damage);
        }
    }

    /// Begin an opacity changed live: how the layer painted on shows kept
    /// aside, the room to record its change held, and, unless it is `settled`
    /// at once, the layers beneath it wanted laid together; `false`, saying
    /// why, where it may not change.
    fn begin_opacity(&mut self, settled: bool, damage: &mut Region) -> bool {
        if !self.editable(damage) {
            return false;
        }
        let found = self
            .document
            .picture()
            .filter(|picture| picture.kind().palette().is_none())
            .and_then(|picture| {
                let layer = picture.active();
                Some((layer, Shown::of(picture.layers().get(layer)?)))
            });
        let Some((layer, before)) = found else {
            return false;
        };
        let (Ok(before), Ok(())) = (before, self.document.reserve()) else {
            self.state(NO_ROOM, damage);
            return false;
        };
        self.layer_looks.edit = Some(OpacityEdit {
            layer,
            before,
            wants_under: !settled,
        });
        true
    }

    /// The layers beneath the one whose opacity is changing, to be laid
    /// together on the preview worker: wanted once the opacity is a change,
    /// where there are any, until a composite of the picture as it stands is
    /// held.
    pub(super) fn wanted_under(&self) -> Option<(Asking, PreviewWork)> {
        let layer = self
            .layer_looks
            .edit
            .as_ref()
            .filter(|edit| edit.wants_under)?
            .layer;
        if layer == 0 || self.composite_under().is_some() {
            return None;
        }
        let basis = self.layer_basis();
        let beneath = self.document.picture()?.layers().get(..layer)?;
        let mut layers = Vec::new();
        layers.try_reserve_exact(beneath.len()).ok()?;
        for held in beneath {
            layers.push(held.try_clone().ok()?);
        }
        Some((Asking::Under(basis), PreviewWork::Under { layers }))
    }

    /// Whether an opacity is being changed live away from how its layer
    /// showed: a change no step records yet.
    pub(super) fn opacity_changing(&self) -> bool {
        let Some(edit) = &self.layer_looks.edit else {
            return false;
        };
        let (layers, _) = slice_of(&self.document);
        layers.get(edit.layer).is_some_and(|layer| {
            (&layer.name, layer.opacity, layer.visible)
                != (&edit.before.name, edit.before.opacity, edit.before.visible)
        })
    }

    /// Land an opacity changed live as one step — none where the layer came
    /// back to how it showed — and let the composite beneath it go.
    pub(super) fn settle_opacity(&mut self, damage: &mut Region) {
        let Some(edit) = self.layer_looks.edit.take() else {
            return;
        };
        self.layer_looks.under = None;
        if let Err(refusal) = self.document.record_shown(edit.layer, edit.before) {
            self.state(&alloc::format!("{refusal}"), damage);
            damage.add(self.layout.canvas());
        }
        damage.add(self.layers.row_rect(edit.layer, self.layout.layers()));
        self.sync_layers(damage);
    }

    /// Let the composite beneath an opacity changing go to memory pressure,
    /// with what the preview worker is laying of it: each paint lays every
    /// layer until the opacity settles.
    pub(super) fn drop_under(&mut self) {
        let Some(edit) = self.layer_looks.edit.as_mut() else {
            return;
        };
        edit.wants_under = false;
        self.layer_looks.under = None;
        self.disown_previews();
    }

    /// The thumbnails the Layers pane's rows in view still lack, up to a
    /// job's worth, as preview work: none is asked again until the picture,
    /// the rows in view or their side move.
    pub(super) fn wanted_thumbnails(&mut self, scale: Scale) -> Option<(Asking, PreviewWork)> {
        let placed = *self.layout.layers();
        if placed.rows.is_empty() {
            return None;
        }
        let side = thumb_side(scale);
        let looked = (
            self.document.generation(),
            self.document.current(),
            side,
            placed.shown(self.layers.offset()),
        );
        if self.layer_looks.seen.as_ref() == Some(&looked) {
            return None;
        }
        let picture = self.document.picture()?;
        let layers = picture.layers();
        let thumbs = &mut self.layer_looks.thumbs;
        thumbs.retain(|revision, _| layers.iter().any(|layer| layer.revision == *revision));
        let mut wanted: Vec<(u64, Canvas)> = Vec::new();
        for display in looked.3.clone() {
            let Some(layer) = layers
                .len()
                .checked_sub(display + 1)
                .and_then(|at| layers.get(at))
            else {
                continue;
            };
            let drawn = thumbs
                .get(&layer.revision)
                .is_some_and(|thumb| thumb.side == side);
            if drawn
                || wanted
                    .iter()
                    .any(|(revision, _)| *revision == layer.revision)
            {
                continue;
            }
            if wanted.len() == THUMBS_PER_JOB || wanted.try_reserve(1).is_err() {
                break;
            }
            let Ok(canvas) = layer.canvas.try_clone() else {
                break;
            };
            wanted.push((layer.revision, canvas));
        }
        let aspect = picture.pixel_aspect();
        if wanted.is_empty() {
            self.layer_looks.seen = Some(looked);
            return None;
        }
        let work = PreviewWork::LayerThumbnails {
            side,
            aspect,
            layers: wanted,
        };
        Some((Asking::Thumbnails(looked), work))
    }

    /// Thumbnails the preview worker drew of the state `looked` at, each kept
    /// by its layer's revision; none drawn, they are asked again once the
    /// picture or the rows in view move.
    pub(super) fn land_thumbnails(
        &mut self,
        looked: Looked,
        side: u32,
        drawn: Thumbnails,
        damage: &mut Region,
    ) {
        if drawn.is_empty() {
            self.layer_looks.seen = Some(looked);
            return;
        }
        for (revision, pixels) in drawn {
            let surface = pixels.and_then(|pixels| Surface::from_rgba8(side, side, &pixels));
            self.layer_looks
                .thumbs
                .insert(revision, Thumb { side, surface });
        }
        self.layer_looks.seen = None;
        damage.add(self.layout.layers().rows);
    }

    /// The preview worker had no room for thumbnails of the state `looked`
    /// at, on its queue or in memory: they are asked again once the picture
    /// or the rows in view move.
    pub(super) fn thumbnails_refused(&mut self, looked: Looked) {
        self.layer_looks.seen = Some(looked);
    }

    /// The layers beneath the one whose opacity is changing, laid together
    /// from `basis`: kept while that opacity still changes over them.
    pub(super) fn land_under(&mut self, basis: (usize, usize, u64), canvas: Canvas) {
        if self.layer_looks.edit.is_some() && basis == self.layer_basis() {
            self.layer_looks.under = Some(Under { basis, canvas });
        }
    }
}
