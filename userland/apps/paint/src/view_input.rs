//! What the window does with input: the pointer on each part of it, keys,
//! menu rows, and the answers its workers send back.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId};
use tairix_controls::{
    wheel_steps, Dialog, DialogAction, FieldGroupAction, FieldRow, Keystroke, SaveChanges,
    ScrollAction, SwatchAction, SwatchMark, ToolActivation, ToolbarOutcome,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{SpriteMode, SpriteName, SpritePalette};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PinchPhase, PointerButton};
use tairix_rng::RandU64;
use tairix_theme::Theme;
use tairix_window::menu::{MenuBuilder, Plate};

use super::{
    Action, Clip, Compute, Computed, Gesture, MenuKind, Modal, NewPicture, Outcome, Own, Pending,
    Request, Settles, Then, View, APP_TITLE, GO_TO_ENTRY, RENAME_ENTRY, SPRITE_SIZE,
};
use crate::canvas::{Canvas, Kind, OutOfMemory, Sample};
use crate::colour::Ink;
use crate::dialog::{Answer, Form, Purpose, Well};
use crate::document::{free_name, Entry, Picture, SpriteInfo, NAME_REFUSAL, SPRITE_STEM};
use crate::history::{Applied, Damage, Unapplied};
use crate::layout::Layout;
use crate::render::write_zoom;
use crate::save::restated;
use crate::selection::Floating;
use crate::shape::{line_pixels, Bounds, Point as Fx, Shape, FX};
use crate::stroke::{Layer, Stroke};
use crate::tool::{strip_item, tool_index, StripItem, Tool, ViewCommand};
use crate::transform::{Depth, Transform, TransformError, Turn};
use crate::viewport::{Zoom, ACTUAL, ZOOMS};

/// What a clearing is said not to have managed.
const CLEARING: &str = "clear that";

/// Why a sprite kept as its bytes takes no edit.
const KEPT_UNCHANGED: &str = "This sprite cannot be edited; it is kept, and saved back unchanged";

impl View {
    /// Feed one pointer event, at monotonic time `now_ns`.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match event {
            InputEvent::PointerMoved { to } => {
                self.pointer = *to;
                self.hovered(layout, damage);
            }
            InputEvent::ModifiersChanged { modifiers } => {
                // Shift squares a shape being dragged, so its preview moves.
                let before = self.preview_bounds();
                self.modifiers = *modifiers;
                if before.is_some() {
                    self.damage_picture(before, layout, damage);
                    self.damage_picture(self.preview_bounds(), layout, damage);
                }
                return Outcome::none();
            }
            _ => {}
        }
        if let Some(outcome) = self.modal_pointer(event, layout, scale, theme, damage) {
            return outcome;
        }
        if let InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        } = event
        {
            if layout.window().contains(self.pointer) {
                // The menu takes the pointer, so the release that would have
                // ended the drag never comes here.
                self.end_gesture(layout, damage);
                return Outcome::asking(Request::Menu {
                    kind: MenuKind::Window,
                    anchor: Rect::new(self.pointer.x, self.pointer.y, 0, 0),
                });
            }
        }
        if let InputEvent::PointerScrolled { dx, dy } = event {
            return self.wheel(*dx, *dy, layout, scale, theme, damage);
        }
        if let InputEvent::Pinch { phase, scale, at } = *event {
            self.pinch(phase, scale, at, layout, damage);
            return Outcome::none();
        }
        if self.gesture.is_none() {
            if let Some(outcome) = self.chrome_pointer(event, now_ns, layout, scale, theme, damage)
            {
                return outcome;
            }
        }
        self.canvas_pointer(event, layout, damage)
    }

    /// Note the picture pixel the pointer is over, for the status band.
    pub(super) fn hovered(&mut self, layout: &Layout, damage: &mut Region) {
        let hover = self
            .viewport
            .pixel_at(self.pointer, self.picture_size(), layout.canvas());
        if hover != self.hover {
            self.hover = hover;
            damage.add(layout.position());
        }
    }

    /// A pointer event while a question is showing, which takes them all.
    fn modal_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let window = layout.window();
        let answer = match self.modal.as_mut()? {
            Modal::Close(dialog) => {
                let bounds = close_rect(dialog, window, scale, theme);
                dialog.on_pointer(event, bounds, scale, theme, damage).map(
                    |DialogAction::ActionActivated { index }| {
                        ModalAnswer::Close(SaveChanges::of(index))
                    },
                )
            }
            Modal::Form(form) => form
                .on_pointer(event, window, scale, theme, damage)
                .map(ModalAnswer::Form),
        };
        Some(match answer {
            Some(answer) => self.answer_modal(answer, layout, damage),
            None => Outcome::none(),
        })
    }

    /// The pointer on the window's chrome: the toolbar, the panel, the bars
    /// and the status band. `None` where it is on none of them.
    fn chrome_pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        match self
            .toolbar
            .on_pointer(event, layout.tools(), scale, theme, damage)
        {
            ToolbarOutcome::Activated(action) if action.part == ToolActivation::Primary => {
                return Some(self.strip_activated(action.index, layout, damage));
            }
            ToolbarOutcome::Activated(_) | ToolbarOutcome::Redraw => return Some(Outcome::none()),
            ToolbarOutcome::Idle => {}
        }
        if let Some(outcome) = self.panel_pointer(event, now_ns, layout, scale, theme, damage) {
            return Some(outcome);
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.vertical
                .on_pointer(event, layout.vertical_bar(), scale, theme, damage)
        {
            self.scroll_to(None, Some(offset), layout, damage);
            return Some(Outcome::none());
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.horizontal
                .on_pointer(event, layout.horizontal_bar(), scale, theme, damage)
        {
            self.scroll_to(Some(offset), None, layout, damage);
            return Some(Outcome::none());
        }
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        if pressed && layout.zoom().contains(self.pointer) {
            return Some(Outcome::asking(Request::Menu {
                kind: MenuKind::Zoom,
                anchor: layout.zoom(),
            }));
        }
        None
    }

    /// The pointer on the panel: the wells, the palette and the settings.
    fn panel_pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let settings = self
            .settings
            .layout(layout.settings(), layout.window(), scale, theme);
        let listing = self.settings.rows().iter().any(FieldRow::popup_open);
        if let Some(FieldGroupAction { row, action }) = self
            .settings
            .on_pointer(event, settings, scale, theme, damage)
        {
            if let Some(label) = self.options.adopt(self.tool, row, &action) {
                self.relabel_setting(row, label);
                damage.add(layout.settings());
            }
            return Some(Outcome::none());
        }
        // An open list owns the pointer wherever it reaches: a press on its
        // overhang is the list's, never the canvas's beneath.
        if listing {
            return Some(Outcome::none());
        }
        let pressed =
            |button| matches!(event, InputEvent::PointerPressed { button: b } if *b == button);
        if pressed(PointerButton::Primary) || pressed(PointerButton::Middle) {
            if layout.primary_well().contains(self.pointer) {
                return Some(self.edit_colour(Well::Primary, layout, damage));
            }
            if layout.secondary_well().contains(self.pointer) {
                return Some(self.edit_colour(Well::Secondary, layout, damage));
            }
        }
        if pressed(PointerButton::Middle) {
            if let Some(index) = self.swatches.well_at(layout.swatches(), self.pointer) {
                self.choose_well(SwatchMark::Secondary, index, layout, damage);
                return Some(Outcome::none());
            }
        }
        if pressed(PointerButton::Primary) {
            if let Some(index) = self.swatches.well_at(layout.swatches(), self.pointer) {
                let subject = index as u64;
                let run = self.clicks.register(
                    now_ns,
                    subject,
                    PointerButton::Primary,
                    self.double_click,
                    2,
                );
                if run == 2 {
                    if let Some(Ink::Index(entry)) = self.wells.get(index).copied() {
                        return Some(self.edit_colour(Well::Entry(entry), layout, damage));
                    }
                }
            }
        }
        match self
            .swatches
            .on_pointer(event, layout.swatches(), SwatchMark::Primary, damage)
        {
            Some(SwatchAction::Selected { mark, index }) => {
                self.choose_well(mark, index, layout, damage);
                Some(Outcome::none())
            }
            None => layout.panel().contains(self.pointer).then(Outcome::none),
        }
    }

    /// Give settings row `row` the label `label`.
    fn relabel_setting(&mut self, row: usize, label: String) {
        let focus = self.settings.focus();
        if let Some(held) = self.settings.rows_mut().get_mut(row) {
            *held = tairix_controls::FieldRow::new(label, held.control().clone());
        }
        self.settings.adopt_focus(focus);
    }

    /// Make well `index` the `mark` colour.
    fn choose_well(
        &mut self,
        mark: SwatchMark,
        index: usize,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let Some(ink) = self.wells.get(index).copied() else {
            return;
        };
        match mark {
            SwatchMark::Primary => self.primary = ink,
            SwatchMark::Secondary => self.secondary = ink,
        }
        self.mark_wells();
        damage.add(layout.wells());
        damage.add(layout.swatches());
    }

    fn wheel(
        &mut self,
        dx: i32,
        dy: i32,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        if layout.tools().contains(self.pointer) {
            self.toolbar
                .wheel(dx, dy, layout.tools(), scale, theme, damage);
            return Outcome::none();
        }
        if !layout.canvas().contains(self.pointer) {
            return Outcome::none();
        }
        if self.modifiers.ctrl {
            // A detent's worth of turn away from the user is one rung in; a
            // fine wheel's fractions add up, and a reversal starts afresh.
            let rungs = wheel_steps(dy.saturating_neg(), 1, &mut self.zoom_carry);
            if rungs != 0 {
                let rung = self.viewport.rung_beside(rungs);
                self.zoom_to(rung, self.pointer, layout, damage);
            }
            return Outcome::none();
        }
        let y = self
            .vertical
            .wheel(dx, dy, scale, layout.vertical_bar(), damage);
        let x = self
            .horizontal
            .wheel(dx, dy, scale, layout.horizontal_bar(), damage);
        let offset =
            |action: Option<ScrollAction>| action.map(|ScrollAction::ScrollTo { offset }| offset);
        self.scroll_to(offset(x), offset(y), layout, damage);
        Outcome::none()
    }

    /// A step of a pinch begun over the canvas: the picture zooms by the
    /// fingers' spread and follows their centre, both measured from where the
    /// pinch began, so it accumulates no rounding; a cancelled pinch puts the
    /// view back as it found it. A step with no pinch begun is ignored, as
    /// one whose beginning the desktop could not deliver.
    fn pinch(
        &mut self,
        phase: PinchPhase,
        scale: u32,
        at: Point,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let area = layout.canvas();
        if phase == PinchPhase::Begin {
            self.pinch = area.contains(at).then_some((self.viewport, at));
            return;
        }
        let Some((start, began)) = self.pinch else {
            return;
        };
        if phase.ends() {
            self.pinch = None;
        }
        let before = self.viewport;
        self.viewport = start;
        if phase != PinchPhase::Cancel {
            let picture = self.picture_size();
            self.viewport
                .magnify(start.zoom().scaled(scale), began, picture, area);
            self.viewport.scroll_by(
                i64::from(began.x) - i64::from(at.x),
                i64::from(began.y) - i64::from(at.y),
                picture,
                area,
            );
        }
        if self.viewport != before {
            damage.add(area);
            damage.add(layout.zoom());
        }
        self.settle(layout, damage);
    }

    /// The toolbar's item at `index` was pressed.
    fn strip_activated(&mut self, index: usize, layout: &Layout, damage: &mut Region) -> Outcome {
        match strip_item(index) {
            Some(StripItem::Tool(tool)) => self.act(Action::Tool(tool), layout, damage),
            Some(StripItem::Command(command)) => {
                let action = match command {
                    ViewCommand::ZoomIn => Action::ZoomIn,
                    ViewCommand::ZoomOut => Action::ZoomOut,
                    ViewCommand::Fit => Action::Fit,
                    ViewCommand::Actual => Action::Actual,
                    ViewCommand::Grid => Action::Grid,
                };
                self.act(action, layout, damage)
            }
            None => Outcome::none(),
        }
    }

    /// Choose `tool`, forgetting the selection when leaving the select tool:
    /// a floating layer was put down before this was reached.
    fn choose_tool(&mut self, tool: Tool, layout: &Layout, damage: &mut Region) -> Outcome {
        if tool == self.tool {
            return Outcome::none();
        }
        if tool != Tool::Select {
            self.drop_selection(layout, damage);
        }
        // A settings popup open over the canvas goes with the panel it hung
        // from, so only then is more than the toolbar and the panel drawn.
        let popup = self.settings.rows().iter().any(FieldRow::popup_open);
        self.tool = tool;
        self.toolbar.set_active(tool_index(tool));
        self.settings = self.options.panel(tool, self.kind().sample_bytes() == 4);
        if popup {
            return Outcome::relaid();
        }
        damage.add(layout.toolbar());
        damage.add(layout.panel());
        Outcome::reshaped()
    }

    /// The pointer on the canvas.
    fn canvas_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        match event {
            // A drag belongs to the button that began it: another pressed
            // meanwhile neither ends it nor begins one of its own.
            InputEvent::PointerPressed { button }
                if self.gesture.is_none() && layout.canvas().contains(self.pointer) =>
            {
                let secondary = match button {
                    PointerButton::Primary => false,
                    PointerButton::Middle => true,
                    PointerButton::Secondary => return Outcome::none(),
                };
                let outcome = self.press(secondary, layout, damage);
                self.dragging = self.gesture.is_some().then_some(*button);
                outcome
            }
            InputEvent::PointerMoved { .. } => {
                self.drag(layout, damage);
                Outcome::none()
            }
            InputEvent::PointerReleased { button } if self.dragging == Some(*button) => {
                self.release(layout, damage);
                Outcome::none()
            }
            _ => Outcome::none(),
        }
    }

    /// Where the pointer is on the picture, in picture units.
    fn at(&self, layout: &Layout) -> Fx {
        let (x, y) = self
            .viewport
            .to_picture(self.pointer, self.picture_size(), layout.canvas());
        Fx { x, y }
    }

    /// Refuse a change to the document, or to the entry showing, while a
    /// worker has the picture, saying why: its answer is written over the
    /// state it was asked of.
    fn idle(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if self.pending.is_none() {
            return true;
        }
        self.state("Wait: the picture is being worked on", layout, damage);
        false
    }

    /// Refuse an edit while the picture waits on a worker or is not a
    /// picture at all, saying why.
    fn editable(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if !self.idle(layout, damage) {
            return false;
        }
        if self.document.picture().is_none() {
            self.state(KEPT_UNCHANGED, layout, damage);
            return false;
        }
        true
    }

    fn state(&mut self, message: &str, layout: &Layout, damage: &mut Region) {
        self.message = Some(String::from(message));
        damage.add(layout.message());
    }

    /// A press on the canvas: begin what the tool does.
    fn press(&mut self, secondary: bool, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let at = self.at(layout);
        if self.modifiers.alt || self.tool == Tool::Picker {
            self.pick(at, secondary, layout, damage);
            return Outcome::none();
        }
        match self.tool {
            Tool::Select => self.select_press(at.pixel(), layout, damage),
            Tool::Fill => self.fill_at(at, secondary, layout, damage),
            tool if tool.shaped() => {
                if self.reserve(layout, damage) {
                    self.gesture = Some(Gesture::Shape {
                        from: at,
                        to: at,
                        secondary,
                    });
                }
                Outcome::none()
            }
            _ => {
                self.begin_stroke(at, secondary, layout, damage);
                Outcome::none()
            }
        }
    }

    /// Make room in the history for the change about to be made, saying so
    /// where there is none.
    fn reserve(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if self.document.reserve().is_ok() {
            return true;
        }
        self.state(
            "There is not enough memory to keep this change; nothing was drawn",
            layout,
            damage,
        );
        false
    }

    /// Take the colour at `at` as the primary or secondary colour.
    fn pick(&mut self, at: Fx, secondary: bool, layout: &Layout, damage: &mut Region) {
        let (x, y) = at.pixel();
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return;
        };
        let Some(picture) = self.document.picture() else {
            return;
        };
        let Some(sample) = picture.canvas.sample(x, y) else {
            return;
        };
        let ink = Ink::of_sample(sample, picture.canvas.kind());
        if secondary {
            self.secondary = ink;
        } else {
            self.primary = ink;
        }
        self.mark_wells();
        damage.add(layout.wells());
        damage.add(layout.swatches());
    }

    fn begin_stroke(&mut self, at: Fx, secondary: bool, layout: &Layout, damage: &mut Region) {
        if !self.reserve(layout, damage) {
            return;
        }
        let kind = self.kind();
        let smooth = self.smooth(kind) && self.tool != Tool::Pencil && self.tool != Tool::Spray;
        let ink = match self.tool {
            Tool::Eraser => self.eraser_ink(kind),
            _ => self.ink(secondary),
        };
        let layer = Self::layer(ink, kind, smooth || self.tool == Tool::Spray);
        // What was said before this stroke is done with; what it says itself
        // stays once it ends.
        if self.message.take().is_some() {
            damage.add(layout.message());
        }
        self.gesture = Some(Gesture::Stroke {
            stroke: Stroke::new(layer, None),
            last: at,
        });
        self.stroke_to(at, layout, damage);
    }

    /// Carry the stroke under way on to `to`.
    fn stroke_to(&mut self, to: Fx, layout: &Layout, damage: &mut Region) {
        let tool = self.tool;
        let size = i64::from(self.options.size);
        let aa = self.smooth(self.kind());
        if tool == Tool::Spray {
            if let Some(Gesture::Stroke { last, .. }) = &mut self.gesture {
                *last = to;
            }
            self.spray_at(to, layout, damage);
            return;
        }
        let Some(Gesture::Stroke { stroke, last }) = &mut self.gesture else {
            return;
        };
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let from = *last;
        *last = to;
        let written = if tool == Tool::Pencil {
            let mut outcome = Ok(());
            line_pixels(from.pixel(), to.pixel(), |x, y| {
                if outcome.is_ok() {
                    outcome = stroke.cover_pixel(canvas, 0, x, y);
                }
            });
            outcome
        } else {
            let shape = Shape::Capsule {
                a: from,
                b: to,
                radius: (size * FX / 2).max(FX / 2),
            };
            stroke.cover(canvas, 0, &shape, aa)
        };
        let bounds = stroke.take_damage();
        self.damage_picture(bounds, layout, damage);
        if written.is_err() {
            self.state("There is not enough memory to draw more", layout, damage);
        }
    }

    /// Scatter one burst of spray about `at`.
    pub(crate) fn spray_at(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        let radius = i64::from(self.options.size.max(2)) * FX / 2;
        let area = u64::try_from(radius * radius / (FX * FX))
            .unwrap_or(1)
            .max(1);
        let dots = (area * u64::from(self.options.flow) / 60).clamp(1, 512);
        let Some(Gesture::Stroke { stroke, .. }) = &mut self.gesture else {
            return;
        };
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let reach = u64::try_from(radius * 2 + 1).unwrap_or(1);
        let mut written = Ok(());
        let mut placed = 0;
        let mut tries = 0;
        while placed < dots && tries < dots * 4 && written.is_ok() {
            tries += 1;
            let dx = i64::try_from(self.spray.next_u64() % reach).unwrap_or(0) - radius;
            let dy = i64::try_from(self.spray.next_u64() % reach).unwrap_or(0) - radius;
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            placed += 1;
            let dot = Fx {
                x: at.x + dx,
                y: at.y + dy,
            };
            let (x, y) = dot.pixel();
            written = stroke.cover_pixel(canvas, 0, x, y);
        }
        let bounds = stroke.take_damage();
        self.damage_picture(bounds, layout, damage);
        if written.is_err() {
            self.state("There is not enough memory to draw more", layout, damage);
        }
    }

    /// The pointer moved with a drag under way.
    fn drag(&mut self, layout: &Layout, damage: &mut Region) {
        let at = self.at(layout);
        match self.gesture {
            Some(Gesture::Stroke { .. }) => self.stroke_to(at, layout, damage),
            Some(Gesture::Shape { .. }) => {
                let before = self.preview_bounds();
                if let Some(Gesture::Shape { to, .. }) = &mut self.gesture {
                    *to = at;
                }
                let after = self.preview_bounds();
                self.damage_picture(before, layout, damage);
                self.damage_picture(after, layout, damage);
            }
            Some(Gesture::Marquee { from }) => self.mark_out(from, at.pixel(), layout, damage),
            Some(Gesture::Move { from, last }) => {
                let now = at.pixel();
                self.gesture = Some(Gesture::Move { from, last: now });
                self.shift_floating((now.0 - last.0, now.1 - last.1), layout, damage);
            }
            None => {}
        }
    }

    /// Mark the selection out from pixel `from` to pixel `to`.
    fn mark_out(&mut self, from: (i64, i64), to: (i64, i64), layout: &Layout, damage: &mut Region) {
        let (width, height) = self.picture_size();
        let wanted = Bounds {
            x0: from.0.min(to.0),
            y0: from.1.min(to.1),
            x1: from.0.max(to.0) + 1,
            y1: from.1.max(to.1) + 1,
        }
        .intersection(&Bounds::picture(width, height));
        let before = self.selection;
        self.selection = (!wanted.is_empty()).then_some(wanted);
        self.damage_picture(before, layout, damage);
        self.damage_picture(self.selection, layout, damage);
    }

    /// Shift the floating layer by `(dx, dy)` pixels.
    fn shift_floating(&mut self, (dx, dy): (i64, i64), layout: &Layout, damage: &mut Region) {
        if (dx, dy) == (0, 0) {
            return;
        }
        let Some(held) = &mut self.held else {
            return;
        };
        let before = held.bounds();
        held.shift(dx, dy);
        let after = held.bounds();
        self.damage_picture(Some(before), layout, damage);
        self.damage_picture(Some(after), layout, damage);
    }

    /// The pixels the shape being dragged covers.
    fn preview_bounds(&self) -> Option<Bounds> {
        self.preview()?
            .layers
            .iter()
            .flatten()
            .map(|(_, shape)| shape.bounds())
            .reduce(|a, b| a.union(&b))
    }

    /// The pointer let go: finish the drag.
    fn release(&mut self, layout: &Layout, damage: &mut Region) {
        self.dragging = None;
        match self.gesture.take() {
            Some(Gesture::Stroke { stroke, .. }) => {
                self.document.record_tiles(stroke.finish());
            }
            Some(Gesture::Shape {
                from,
                to,
                secondary,
            }) => self.put_shape(from, to, secondary, layout, damage),
            Some(Gesture::Marquee { .. } | Gesture::Move { .. }) | None => {}
        }
        self.spray_due = None;
    }

    /// Lay the shape dragged from `from` to `to` down on the picture.
    fn put_shape(
        &mut self,
        from: Fx,
        to: Fx,
        secondary: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let kind = self.kind();
        let aa = self.smooth(kind);
        let [Some((first, first_shape)), second] = self.shape_layers(from, to, secondary, kind)
        else {
            return;
        };
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let mut stroke = Stroke::new(first, second.map(|(layer, _)| layer));
        let mut written = stroke.cover(canvas, 0, &first_shape, aa);
        if let (Ok(()), Some((_, shape))) = (&written, second) {
            written = stroke.cover(canvas, 1, &shape, aa);
        }
        let bounds = stroke.take_damage();
        if written.is_ok() {
            self.document.record_tiles(stroke.finish());
        } else {
            stroke.revert(canvas);
            self.state("There is not enough memory to draw that", layout, damage);
        }
        self.damage_picture(bounds, layout, damage);
    }

    /// Fill from `at`, on a worker.
    fn fill_at(
        &mut self,
        at: Fx,
        secondary: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let (x, y) = at.pixel();
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return Outcome::none();
        };
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        if picture.canvas.sample(x, y).is_none() {
            return Outcome::none();
        }
        let ink = self.ink(secondary);
        let layer = Layer {
            ink,
            blend: if ink == Ink::Clear {
                crate::stroke::Blend::Replace
            } else {
                crate::stroke::Blend::Over
            },
        };
        let Ok(canvas) = picture.canvas.try_clone() else {
            self.state("There is not enough memory to fill", layout, damage);
            return Outcome::none();
        };
        let tolerance = self.options.tolerance;
        self.begin_work(
            Compute::Fill {
                canvas,
                at: (x, y),
                tolerance,
                layer,
            },
            None,
            Settles::Nothing,
            "fill",
            layout,
            damage,
        )
    }

    /// Hand `work` to a worker; the picture takes no edits until it is back,
    /// and `settles` says what then becomes of the selection.
    fn begin_work(
        &mut self,
        work: Compute,
        transform: Option<Transform>,
        settles: Settles,
        what: &'static str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let job = self.next_job;
        self.next_job += 1;
        self.pending = Some(Pending {
            job,
            entry: self.document.current(),
            generation: self.document.generation(),
            transform,
            settles,
            what,
        });
        self.state("Working\u{2026}", layout, damage);
        Outcome::asking(Request::Own(Own::Compute { job, work }))
    }

    /// A worker answered job `job`.
    pub fn computed(
        &mut self,
        job: u64,
        answer: Computed,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let Some(pending) = self.pending.take_if(|pending| pending.job == job) else {
            return Outcome::none();
        };
        self.end_gesture(layout, damage);
        self.message = None;
        damage.add(layout.message());
        if (self.document.current(), self.document.generation())
            != (pending.entry, pending.generation)
        {
            self.state(
                "The picture changed while it was being worked on; it was left as it is",
                layout,
                damage,
            );
            return Outcome::none();
        }
        match answer {
            Computed::Tiles(Ok(tiles)) => {
                let filled = self.document.picture().and_then(|picture| {
                    tiles
                        .iter()
                        .map(|(index, _)| picture.canvas.tile_rect(*index).bounds())
                        .reduce(|a, b| a.union(&b))
                });
                let refusal = match self.document.adopt_tiles(tiles) {
                    Ok(true) => {
                        self.damage_picture(filled, layout, damage);
                        return self.settle_selection(pending.settles, layout, damage);
                    }
                    Ok(false) => alloc::format!(
                        "What was asked no longer fits the picture, so it could not {}",
                        pending.what
                    ),
                    Err(OutOfMemory) => {
                        alloc::format!("There is not enough memory to {}", pending.what)
                    }
                };
                self.say(refusal);
            }
            Computed::Tiles(Err(OutOfMemory)) => {
                let reason = alloc::format!("There is not enough memory to {}", pending.what);
                self.say(reason);
            }
            Computed::Picture(Ok(canvas)) => {
                let sprite = self.document.picture().and_then(|held| {
                    let sprite = held.sprite.as_ref()?;
                    Some(match pending.transform {
                        Some(transform) => sprite.refit(transform, &canvas),
                        None => sprite.clone(),
                    })
                });
                match self.document.replace_picture(Picture { canvas, sprite }) {
                    Ok(true) => {}
                    Ok(false) => self.say(KEPT_UNCHANGED),
                    Err(OutOfMemory) => self.say("There is not enough memory to keep the change"),
                }
                return self.after_picture_change(layout, damage);
            }
            Computed::Picture(Err(err)) => {
                let reason = match err {
                    TransformError::OutOfMemory => {
                        alloc::format!("There is not enough memory to {}", pending.what)
                    }
                    TransformError::BadSize => alloc::format!(
                        "The picture cannot {}: it would be too large or empty",
                        pending.what
                    ),
                    TransformError::NotApplicable => {
                        alloc::format!("This picture cannot {}", pending.what)
                    }
                };
                self.say(reason);
            }
        }
        Outcome::none()
    }

    /// A worker's tiles are in: what it settles of the selection follows.
    fn settle_selection(
        &mut self,
        settles: Settles,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        match settles {
            Settles::Nothing => Outcome::none(),
            Settles::Cleared => {
                if let Some(held) = self.held.take() {
                    self.damage_picture(Some(held.bounds()), layout, damage);
                }
                Outcome::none()
            }
            Settles::PutDown(then) => {
                if let Some(held) = self.held.take() {
                    let (width, height) = self.picture_size();
                    let on = held.bounds().intersection(&Bounds::picture(width, height));
                    self.selection = (!on.is_empty()).then_some(on);
                    self.damage_picture(Some(held.bounds()), layout, damage);
                }
                self.carry_on(then, layout, damage)
            }
        }
    }

    /// The picture showing was replaced, or another shown: carry the inks,
    /// the panel and the view over to it.
    fn after_picture_change(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        self.selection = None;
        self.adopt_kind();
        damage.add(layout.window());
        self.settle(layout, damage);
        Outcome::relaid()
    }

    /// A press with the select tool: drag the floating layer, lift the
    /// selection, put the layer down, or mark a new selection out.
    fn select_press(&mut self, at: (i64, i64), layout: &Layout, damage: &mut Region) -> Outcome {
        let inside = |bounds: Bounds| {
            (bounds.x0..bounds.x1).contains(&at.0) && (bounds.y0..bounds.y1).contains(&at.1)
        };
        if self
            .floating()
            .is_some_and(|floating| inside(floating.bounds()))
        {
            self.gesture = Some(Gesture::Move { from: at, last: at });
            return Outcome::none();
        }
        if self.held.is_some() {
            // A press off the layer puts it down; the next marks anew.
            return self.put_down_then(Then::Rest, layout, damage);
        }
        if self.selection.is_some_and(inside) {
            if self.lift(layout, damage) {
                self.gesture = Some(Gesture::Move { from: at, last: at });
            }
            return Outcome::none();
        }
        self.drop_selection(layout, damage);
        self.gesture = Some(Gesture::Marquee { from: at });
        Outcome::none()
    }

    /// Lift the selection into a floating layer, leaving what an eraser
    /// would once it is put down. Nothing is written or copied.
    fn lift(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        let Some(area) = self.selection else {
            return false;
        };
        let left = self.eraser_ink(self.kind());
        let Some(picture) = self.document.picture() else {
            return false;
        };
        let Ok(floating) = Floating::lift(&picture.canvas, area, left) else {
            self.state(
                "There is not enough memory to lift the selection",
                layout,
                damage,
            );
            return false;
        };
        self.held = Some(floating);
        self.selection = None;
        self.damage_picture(Some(area), layout, damage);
        true
    }

    /// Put the floating layer down on a worker, then carry out `then` once it
    /// has landed; with nothing floating, carry it out now. The picture takes
    /// no edits until the layer is down.
    fn put_down_then(&mut self, then: Then, layout: &Layout, damage: &mut Region) -> Outcome {
        if self.held.is_none() {
            return self.carry_on(then, layout, damage);
        }
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let shared = self
            .document
            .picture()
            .zip(self.held.as_ref())
            .map(|(picture, held)| (picture.canvas.try_clone(), held.try_clone()));
        let Some((Ok(canvas), Ok(floating))) = shared else {
            self.state(
                "There is not enough memory to put the selection down",
                layout,
                damage,
            );
            return Outcome::none();
        };
        self.begin_work(
            Compute::PutDown { canvas, floating },
            None,
            Settles::PutDown(then),
            "put the selection down",
            layout,
            damage,
        )
    }

    /// Carry out what follows a floating layer's putting down.
    fn carry_on(&mut self, then: Then, layout: &Layout, damage: &mut Region) -> Outcome {
        match then {
            Then::Rest => Outcome::none(),
            Then::Act(action) => self.act(action, layout, damage),
            Then::Float(floating) => self.float(floating, layout, damage),
            Then::SaveThenClose => Outcome::asking(Request::SaveThenClose),
        }
    }

    /// Float `floating` over the picture, the select tool taken up to move it.
    fn float(&mut self, floating: Floating, layout: &Layout, damage: &mut Region) -> Outcome {
        self.drop_selection(layout, damage);
        let bounds = floating.bounds();
        self.held = Some(floating);
        self.damage_picture(Some(bounds), layout, damage);
        self.choose_tool(Tool::Select, layout, damage)
    }

    /// Forget the selection marked out.
    fn drop_selection(&mut self, layout: &Layout, damage: &mut Region) {
        if let Some(selection) = self.selection.take() {
            self.damage_picture(Some(selection), layout, damage);
        }
    }

    /// Turn a floating layer down: one lifted goes back where it was, one
    /// pasted is thrown away. Nothing was written, so nothing is undone.
    fn turn_down_floating(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if self.held.is_none() {
            return false;
        }
        if !self.idle(layout, damage) {
            return true;
        }
        if let Some(held) = self.held.take() {
            self.damage_picture(held.lifted_from(), layout, damage);
            self.damage_picture(Some(held.bounds()), layout, damage);
        }
        true
    }

    /// End any drag, as though the pointer had let go.
    pub(crate) fn end_gesture(&mut self, layout: &Layout, damage: &mut Region) {
        if self.gesture.is_some() {
            self.release(layout, damage);
        }
    }

    /// Turn the drag under way down, keeping nothing it did: a stroke is
    /// taken back off the picture rather than left there unrecorded, a
    /// marquee marks nothing, and a dragged layer goes back where it was.
    fn cancel_gesture(&mut self, layout: &Layout, damage: &mut Region) {
        let preview = self.preview_bounds();
        self.dragging = None;
        self.spray_due = None;
        match self.gesture.take() {
            Some(Gesture::Stroke { stroke, .. }) => {
                if let Some(canvas) = self.document.canvas_mut() {
                    let restored = stroke.revert(canvas);
                    self.damage_picture(restored, layout, damage);
                }
            }
            Some(Gesture::Marquee { .. }) => {
                let marked = self.selection.take();
                self.damage_picture(marked, layout, damage);
            }
            Some(Gesture::Move { from, last }) => {
                self.shift_floating((from.0 - last.0, from.1 - last.1), layout, damage);
            }
            Some(Gesture::Shape { .. }) | None => {}
        }
        self.damage_picture(preview, layout, damage);
    }

    /// The area an edit of the selection acts on: the floating layer's, else
    /// the selection's.
    fn selected_area(&self) -> Option<Bounds> {
        self.floating().map(Floating::bounds).or(self.selection)
    }

    /// What a copy of the selection takes, as it stands: `None` with nothing
    /// selected.
    fn clip(&self) -> Option<Result<Clip, OutOfMemory>> {
        if let Some(floating) = &self.held {
            return Some(floating.try_clone().map(Clip::Floating));
        }
        let area = self.selection?;
        let picture = self.document.picture()?;
        Some(
            picture
                .canvas
                .try_clone()
                .map(|canvas| Clip::Area { canvas, area }),
        )
    }

    /// The clipboard answered a paste with `pasted`, a picture decoded from
    /// it as a layer over a picture of the kind it names, or the reason there
    /// is none: float it over the picture's visible top left.
    pub fn pasted(
        &mut self,
        pasted: Result<(Canvas, Kind), String>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let canvas = match pasted {
            Ok((canvas, kind)) if kind == *self.kind() => canvas,
            Ok(_) => {
                self.state(
                    "The picture changed while it was pasted; paste again",
                    layout,
                    damage,
                );
                return Outcome::none();
            }
            Err(reason) => {
                self.state(&reason, layout, damage);
                return Outcome::none();
            }
        };
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        self.end_gesture(layout, damage);
        let area = layout.canvas();
        let corner = self.viewport.to_picture(
            Point::new(area.left(), area.top()),
            self.picture_size(),
            area,
        );
        let at = (
            corner.0.div_euclid(FX).max(0),
            corner.1.div_euclid(FX).max(0),
        );
        self.put_down_then(Then::Float(Floating::pasted(canvas, at)), layout, damage)
    }

    /// The user asked to close the window.
    pub fn close_requested(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        self.end_gesture(layout, damage);
        if !tairix_window::document::SavedDocument::is_modified(self) {
            return Outcome::asking(Request::Close);
        }
        self.modal = Some(Modal::Close(Dialog::save_changes(&self.name)));
        damage.add(layout.window());
        Outcome::none()
    }

    fn answer_modal(
        &mut self,
        answer: ModalAnswer,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        damage.add(layout.window());
        match (self.modal.take(), answer) {
            (Some(Modal::Close(_)), ModalAnswer::Close(SaveChanges::Discard)) => {
                Outcome::asking(Request::Close)
            }
            (Some(Modal::Close(_)), ModalAnswer::Close(SaveChanges::Save)) => {
                self.put_down_then(Then::SaveThenClose, layout, damage)
            }
            (Some(Modal::Form(form)), ModalAnswer::Form(Answer::Confirmed)) => {
                self.form_answered(form, layout, damage)
            }
            _ => Outcome::none(),
        }
    }

    /// A form was accepted: carry out what it says, or state why not and
    /// leave it open.
    fn form_answered(&mut self, form: Box<Form>, layout: &Layout, damage: &mut Region) -> Outcome {
        let refused = |view: &mut Self, mut form: Box<Form>, reason: &str| {
            form.refuse(reason);
            view.modal = Some(Modal::Form(form));
            Outcome::none()
        };
        match form.purpose() {
            Purpose::NewPicture => match form.new_picture_answer() {
                Ok(picture) => Outcome::asking(Request::Own(Own::NewWindow(picture))),
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::NewSprite => match form.new_sprite_answer() {
                Ok(new) if self.document.names_taken(&new.name, None) => {
                    refused(self, form, "A sprite of that name is already here")
                }
                Ok(new) => self.add_sprite(new, layout, damage),
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::Scale => match form.scale_answer() {
                Ok(((width, height), smooth)) => self.transform(
                    Transform::Scale {
                        width,
                        height,
                        smooth,
                    },
                    "resize",
                    layout,
                    damage,
                ),
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::Canvas => match form.canvas_answer() {
                Ok(((width, height), anchor)) => {
                    let fill = self.fill_sample();
                    self.transform(
                        Transform::Resize {
                            width,
                            height,
                            anchor,
                            fill,
                        },
                        "change size",
                        layout,
                        damage,
                    )
                }
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::Convert => {
                let (depth, palette, dither) = form.convert_answer();
                let depth = depth.map_or(Depth::Rgba, Depth::Indexed);
                self.transform(
                    Transform::Convert {
                        depth,
                        palette,
                        dither,
                    },
                    "change its colours",
                    layout,
                    damage,
                )
            }
            Purpose::Quality => {
                self.document.set_jpeg_quality(form.quality_answer());
                Outcome::none()
            }
            Purpose::Colour { well, .. } => {
                let colour = form.colour_answer();
                self.set_colour(well, colour, layout, damage);
                Outcome::none()
            }
        }
    }

    /// What a new pixel of the canvas is: the secondary colour where the
    /// picture cannot be clear, else nothing.
    fn fill_sample(&self) -> Sample {
        let kind = self.kind();
        match (self.eraser_ink(kind), &kind) {
            (Ink::Clear, Kind::Rgba) => Sample::Rgba([0; 4]),
            (Ink::Clear, Kind::Indexed { .. }) => Sample::Index(0, 0),
            (Ink::Index(index), _) => Sample::Index(index, u8::MAX),
            (Ink::Colour(colour), _) => Sample::Rgba(colour),
        }
    }

    /// Set `well` to `colour`: an ink, or a palette entry, which is a change
    /// to the picture.
    fn set_colour(&mut self, well: Well, colour: [u8; 4], layout: &Layout, damage: &mut Region) {
        let kind = self.kind();
        let entry = match (well, &kind) {
            (Well::Entry(index), _) => Some(index),
            (Well::Primary | Well::Secondary, Kind::Indexed { .. }) => {
                match self.ink(well == Well::Secondary) {
                    Ink::Index(index) => Some(index),
                    _ => None,
                }
            }
            (_, Kind::Rgba) => None,
        };
        if let (Some(index), Some(palette)) = (entry, kind.palette()) {
            let mut palette = palette.to_vec();
            if let Some(slot) = palette.get_mut(usize::from(index)) {
                *slot = colour;
                self.change_palette(palette, layout, damage);
            }
            return;
        }
        let ink = if colour[3] == 0 {
            Ink::Clear
        } else {
            Ink::Colour(colour)
        };
        match well {
            Well::Secondary => self.secondary = ink,
            _ => self.primary = ink,
        }
        self.mark_wells();
        damage.add(layout.wells());
        damage.add(layout.swatches());
    }

    /// Give the palette picture showing `palette`, as one step, its sprite
    /// details restating it.
    fn change_palette(&mut self, palette: Vec<[u8; 4]>, layout: &Layout, damage: &mut Region) {
        if !self.idle(layout, damage) {
            return;
        }
        let kind = self.kind();
        let sprite = self.document.picture().and_then(|picture| {
            let mut sprite = picture.sprite.clone()?;
            sprite.palette = restated(&Kind::Indexed {
                depth: kind.depth()?,
                palette: palette.clone(),
                masked: kind.masked(),
            });
            Some(sprite)
        });
        match self.document.set_details(Some(palette), sprite) {
            Ok(true) => {
                self.adopt_kind();
                damage.add(layout.window());
            }
            Ok(false) => {}
            Err(OutOfMemory) => self.state(
                "There is not enough memory to change the palette",
                layout,
                damage,
            ),
        }
    }

    /// Put `form` over the window.
    fn ask(&mut self, form: Form, layout: &Layout, damage: &mut Region) {
        // A form answers the picture as it stood when it opened, so none opens
        // while a worker may yet change it.
        if !self.idle(layout, damage) {
            return;
        }
        self.end_gesture(layout, damage);
        self.modal = Some(Modal::Form(Box::new(form)));
        damage.add(layout.window());
    }

    /// Open the colour form for `well`.
    fn edit_colour(&mut self, well: Well, layout: &Layout, damage: &mut Region) -> Outcome {
        let kind = self.kind();
        let colour = match well {
            Well::Primary => self.primary.shown(kind),
            Well::Secondary => self.secondary.shown(kind),
            Well::Entry(index) => Ink::Index(index).shown(kind),
        };
        let editing_entry = matches!(well, Well::Entry(_))
            || (kind.palette().is_some()
                && matches!(self.ink(well == Well::Secondary), Ink::Index(_)));
        if kind.palette().is_some() && !editing_entry {
            self.state(
                "Choose a colour from the palette, or edit a palette colour",
                layout,
                damage,
            );
            return Outcome::none();
        }
        // A palette entry is translucent only in a picture that is not a
        // sprite: a sprite's palette is colours, its mask the transparency.
        let alpha = if editing_entry {
            self.document
                .picture()
                .is_some_and(|picture| picture.sprite.is_none())
        } else {
            *kind == Kind::Rgba
        };
        self.ask(Form::colour(well, colour, alpha), layout, damage);
        Outcome::none()
    }

    /// Ask a worker for `transform` of the picture showing.
    fn transform(
        &mut self,
        transform: Transform,
        what: &'static str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        let Ok(canvas) = picture.canvas.try_clone() else {
            self.state(
                &alloc::format!("There is not enough memory to {what}"),
                layout,
                damage,
            );
            return Outcome::none();
        };
        self.begin_work(
            Compute::Transform { canvas, transform },
            Some(transform),
            Settles::Nothing,
            what,
            layout,
            damage,
        )
    }

    /// Add the sprite `new` describes after the one showing.
    fn add_sprite(
        &mut self,
        new: crate::dialog::NewSprite,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let blank = NewPicture {
            size: new.size,
            depth: new.depth,
            transparent: new.masked,
        };
        let Ok(canvas) = blank.canvas() else {
            self.state("There is not enough memory for the sprite", layout, damage);
            return Outcome::none();
        };
        let mode = match new.depth {
            Some(depth) => SpriteMode::indexed(depth, new.eig, false),
            None => SpriteMode::truecolour(new.eig, false),
        };
        let sprite = SpriteInfo {
            name: new.name,
            mode,
            palette: SpritePalette::Implied,
            masked: new.masked,
        };
        let entry = Entry::Picture(Picture {
            canvas,
            sprite: Some(sprite),
        });
        let at = self.document.current() + 1;
        match self.document.insert(at, entry) {
            Ok(()) => self.after_picture_change(layout, damage),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
    }

    /// Show entry `index`: a floating layer was put down before this was
    /// reached.
    fn show(&mut self, index: usize, layout: &Layout, damage: &mut Region) -> Outcome {
        if index == self.document.current() || index >= self.document.entries().len() {
            return Outcome::none();
        }
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        self.end_gesture(layout, damage);
        self.document.select(index);
        self.after_picture_change(layout, damage)
    }

    /// Feed one key press.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        self.modifiers = stroke.modifiers;
        if let Some(modal) = &mut self.modal {
            let window = layout.window();
            let answer = match modal {
                Modal::Close(dialog) => match stroke.key {
                    Key::Named(NamedKey::Escape) => Some(ModalAnswer::Close(SaveChanges::Cancel)),
                    key => {
                        damage.add(close_rect(dialog, window, scale, theme));
                        dialog
                            .on_key(key)
                            .map(|DialogAction::ActionActivated { index }| {
                                ModalAnswer::Close(SaveChanges::of(index))
                            })
                    }
                },
                Modal::Form(form) => form
                    .on_key(stroke, window, scale, theme, damage)
                    .map(ModalAnswer::Form),
            };
            return match answer {
                Some(answer) => self.answer_modal(answer, layout, damage),
                None => Outcome::none(),
            };
        }
        match shortcut(stroke.key, stroke.modifiers) {
            Some(action) => self.act(action, layout, damage),
            None => self.plain_key(stroke.key, layout, damage),
        }
    }

    /// A key no shortcut claims: turn a drag or a selection down, nudge a
    /// floating layer, or step the palette.
    fn plain_key(&mut self, key: Key, layout: &Layout, damage: &mut Region) -> Outcome {
        if key == Key::Named(NamedKey::Escape) {
            self.escape(layout, damage);
            return Outcome::none();
        }
        let step = if self.modifiers.shift { 10 } else { 1 };
        let nudge = match key {
            Key::Named(NamedKey::Left) => Some((-step, 0)),
            Key::Named(NamedKey::Right) => Some((step, 0)),
            Key::Named(NamedKey::Up) => Some((0, -step)),
            Key::Named(NamedKey::Down) => Some((0, step)),
            _ => None,
        };
        if let (Some((dx, dy)), Some(held)) = (nudge, &mut self.held) {
            // A layer being put down is put down where it stood when asked.
            if self.pending.is_none() {
                let before = held.bounds();
                held.shift(dx, dy);
                let after = held.bounds();
                self.damage_picture(Some(before), layout, damage);
                self.damage_picture(Some(after), layout, damage);
            }
            return Outcome::none();
        }
        if let Some(SwatchAction::Selected { mark, index }) =
            self.swatches.on_key(key, layout.swatches(), damage)
        {
            self.choose_well(mark, index, layout, damage);
        }
        Outcome::none()
    }

    /// A menu row was chosen.
    pub fn chosen(&mut self, id: AppMenuItemId, layout: &Layout, damage: &mut Region) -> Outcome {
        match Action::from_id(id.get()) {
            Some(action) => self.act(action, layout, damage),
            None => Outcome::none(),
        }
    }

    /// A menu's entry field `id` was committed holding `text`.
    pub fn entered(
        &mut self,
        id: AppMenuItemId,
        text: &str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        self.end_gesture(layout, damage);
        match id.get() {
            GO_TO_ENTRY => self.go_to(text, layout, damage),
            RENAME_ENTRY => {
                self.rename(text, layout, damage);
                Outcome::none()
            }
            _ => Outcome::none(),
        }
    }

    /// Show the sprite `text` names, or numbers from one.
    fn go_to(&mut self, text: &str, layout: &Layout, damage: &mut Region) -> Outcome {
        let text = text.trim();
        let index = SpriteName::new(text)
            .and_then(|name| self.document.find(&name))
            .or_else(|| {
                text.parse::<usize>()
                    .ok()
                    .and_then(|number| number.checked_sub(1))
                    .filter(|&index| index < self.document.entries().len())
            });
        if let Some(index) = index {
            return self.show(index, layout, damage);
        }
        self.state("There is no sprite of that name or number", layout, damage);
        Outcome::none()
    }

    /// Rename the sprite showing `text`.
    fn rename(&mut self, text: &str, layout: &Layout, damage: &mut Region) {
        if !self.idle(layout, damage) {
            return;
        }
        let Some(name) = SpriteName::new(text.trim()) else {
            self.state(NAME_REFUSAL, layout, damage);
            return;
        };
        if self
            .document
            .names_taken(&name, Some(self.document.current()))
        {
            self.state("A sprite of that name is already here", layout, damage);
            return;
        }
        let Some(picture) = self.document.picture() else {
            self.state("A kept sprite keeps its name", layout, damage);
            return;
        };
        let sprite = picture.sprite.as_ref().map_or_else(
            || SpriteInfo::for_canvas(name, &picture.canvas),
            |sprite| SpriteInfo {
                name,
                ..sprite.clone()
            },
        );
        if self.document.set_details(None, Some(sprite)).is_ok() {
            damage.add(layout.sprite());
        } else {
            self.state("There is not enough memory to rename it", layout, damage);
        }
    }

    /// Carry out `action`.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm an action; splitting it would part each action from its siblings"
    )]
    pub fn act(&mut self, action: Action, layout: &Layout, damage: &mut Region) -> Outcome {
        // Whatever the action, a drag under way is finished first: none acts
        // on a picture a stroke is still being laid on.
        self.end_gesture(layout, damage);
        if self.held.is_some() && !action.leaves_floating() {
            return self.put_down_then(Then::Act(action), layout, damage);
        }
        let area = layout.canvas();
        match action {
            Action::NewPicture => {
                self.ask(Form::new_picture(self.picture_size()), layout, damage);
            }
            Action::Open => return Outcome::asking(Request::Open),
            Action::Save => return Outcome::asking(Request::Save),
            Action::SaveAs => return Outcome::asking(Request::SaveAs),
            Action::Quality => {
                let quality = self.document.jpeg_quality();
                self.ask(Form::quality(quality), layout, damage);
            }
            Action::Close => return self.close_requested(layout, damage),
            Action::Undo | Action::Redo => {
                return self.undo_redo(action == Action::Undo, layout, damage)
            }
            Action::Cut => return self.cut(layout, damage),
            Action::Copy => return self.copy(layout, damage),
            Action::Paste => {
                if self.editable(layout, damage) {
                    return Outcome::asking(Request::Own(Own::Paste(self.kind().clone())));
                }
            }
            Action::SelectAll => {
                let (width, height) = self.picture_size();
                self.selection = Some(Bounds::picture(width, height));
                damage.add(area);
                return self.choose_tool(Tool::Select, layout, damage);
            }
            Action::Deselect => self.drop_selection(layout, damage),
            Action::Delete => return self.delete_selection(layout, damage),
            Action::Crop => return self.crop(layout, damage),
            Action::Resize => {
                let smooth = *self.kind() == Kind::Rgba;
                self.ask(Form::scale(self.picture_size(), smooth), layout, damage);
            }
            Action::CanvasSize => {
                self.ask(Form::canvas(self.picture_size()), layout, damage);
            }
            Action::RotateLeft => {
                return self.transform(Transform::Turn(Turn::ThreeQuarters), "turn", layout, damage)
            }
            Action::RotateRight => {
                return self.transform(Transform::Turn(Turn::Quarter), "turn", layout, damage)
            }
            Action::RotateHalf => {
                return self.transform(Transform::Turn(Turn::Half), "turn", layout, damage)
            }
            Action::FlipAcross => {
                return self.transform(
                    Transform::Flip { vertical: false },
                    "mirror",
                    layout,
                    damage,
                )
            }
            Action::FlipDown => {
                return self.transform(Transform::Flip { vertical: true }, "mirror", layout, damage)
            }
            Action::Invert => return self.invert(layout, damage),
            Action::Convert => {
                self.ask(Form::convert(self.kind().depth()), layout, damage);
            }
            Action::AddMask | Action::RemoveMask => {
                let fill = match self.secondary {
                    Ink::Index(index) => index,
                    _ => 0,
                };
                let on = action == Action::AddMask;
                return self.transform(
                    Transform::Mask { on, fill },
                    "change its mask",
                    layout,
                    damage,
                );
            }
            Action::EditPrimary => return self.edit_colour(Well::Primary, layout, damage),
            Action::EditSecondary => return self.edit_colour(Well::Secondary, layout, damage),
            Action::SwapColours => {
                core::mem::swap(&mut self.primary, &mut self.secondary);
                self.mark_wells();
                damage.add(layout.wells());
                damage.add(layout.swatches());
            }
            Action::PreviousSprite => {
                let current = self.document.current();
                return self.show(current.saturating_sub(1), layout, damage);
            }
            Action::NextSprite => {
                let current = self.document.current();
                return self.show(current + 1, layout, damage);
            }
            Action::NewSprite => {
                let name = SpriteName::new(SPRITE_STEM)
                    .and_then(|stem| free_name(&stem, self.document.names()))
                    .map_or_else(String::new, |name| name.to_string());
                self.ask(Form::new_sprite(&name, SPRITE_SIZE), layout, damage);
            }
            Action::DuplicateSprite => return self.duplicate(layout, damage),
            Action::DeleteSprite => return self.delete_sprite(layout, damage),
            Action::SpriteUp | Action::SpriteDown => {
                let current = self.document.current();
                let to = if action == Action::SpriteUp {
                    current.checked_sub(1)
                } else {
                    Some(current + 1).filter(|&to| to < self.document.entries().len())
                };
                if let Some(to) = to.filter(|_| self.idle(layout, damage)) {
                    match self.document.move_entry(current, to) {
                        Ok(()) => damage.add(layout.sprite()),
                        Err(refusal) => self.state(&alloc::format!("{refusal}"), layout, damage),
                    }
                }
            }
            Action::ZoomIn => {
                let rung = self.viewport.rung_beside(1);
                self.zoom_to(rung, self.pointer, layout, damage);
            }
            Action::ZoomOut => {
                let rung = self.viewport.rung_beside(-1);
                self.zoom_to(rung, self.pointer, layout, damage);
            }
            Action::Zoom(rung) => self.zoom_to(rung, area.center(), layout, damage),
            Action::Fit => self.fit(layout, damage),
            Action::Actual => self.zoom_to(ACTUAL, self.pointer, layout, damage),
            Action::Grid => {
                self.grid = !self.grid;
                damage.add(area);
            }
            // Put down before this was reached, which is all the action asks.
            Action::PutDown | Action::GoTo | Action::Rename => {}
            Action::Tool(tool) => return self.choose_tool(tool, layout, damage),
        }
        Outcome::none()
    }

    fn undo_redo(&mut self, undo: bool, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let applied = if undo {
            self.document.undo()
        } else {
            self.document.redo()
        };
        let refusal = match applied {
            Ok(Applied {
                damage: Damage::Area(bounds),
                ..
            }) => {
                self.damage_picture(Some(bounds), layout, damage);
                return Outcome::none();
            }
            Ok(_) => return self.after_picture_change(layout, damage),
            Err(Unapplied::Nothing) => return Outcome::none(),
            Err(Unapplied::NoMemory) => "There is not enough memory for that; nothing changed",
            Err(Unapplied::Stale) => "That change no longer fits the picture, and is gone",
        };
        self.state(refusal, layout, damage);
        Outcome::none()
    }

    fn copy(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        match self.clip() {
            Some(Ok(clip)) => Outcome::asking(Request::Own(Own::Copy(clip))),
            Some(Err(OutOfMemory)) => {
                self.state("There is not enough memory to copy that", layout, damage);
                Outcome::none()
            }
            None => {
                self.state("Select part of the picture to copy", layout, damage);
                Outcome::none()
            }
        }
    }

    /// Copy what is selected, then clear it: one request, the copy taken from
    /// the picture as it stood before the clearing.
    fn cut(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let copied = self.copy(layout, damage);
        let Some(Request::Own(Own::Copy(clip))) = copied.request else {
            return copied;
        };
        let Some(work) = self.clearing(layout, damage) else {
            return Outcome::asking(Request::Own(Own::Copy(clip)));
        };
        let cleared = self.begin_work(work, None, Settles::Cleared, CLEARING, layout, damage);
        let Some(Request::Own(Own::Compute { job, work })) = cleared.request else {
            return Outcome::asking(Request::Own(Own::Copy(clip)));
        };
        Outcome::asking(Request::Own(Own::Cut { clip, job, work }))
    }

    /// Clear what is selected, as an eraser would; a floating layer is
    /// thrown away, and where it was lifted from keeps what the lift leaves.
    fn delete_selection(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        match self.clearing(layout, damage) {
            Some(work) => self.begin_work(work, None, Settles::Cleared, CLEARING, layout, damage),
            None => Outcome::none(),
        }
    }

    /// The worker's clearing of what is selected, or `None` with nothing for
    /// a worker to do: a pasted layer is simply thrown away, and a clearing
    /// that cannot be asked says why.
    fn clearing(&mut self, layout: &Layout, damage: &mut Region) -> Option<Compute> {
        if !self.editable(layout, damage) {
            return None;
        }
        let (area, ink) = if let Some(held) = &self.held {
            let Some(lifted) = held.lifted() else {
                let bounds = held.bounds();
                self.held = None;
                self.damage_picture(Some(bounds), layout, damage);
                return None;
            };
            lifted
        } else {
            (self.selection?, self.eraser_ink(self.kind()))
        };
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas.try_clone())
        else {
            self.state("There is not enough memory to clear that", layout, damage);
            return None;
        };
        Some(Compute::Clear { canvas, area, ink })
    }

    fn crop(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(area) = self.selection else {
            self.state("Select the part of the picture to keep", layout, damage);
            return Outcome::none();
        };
        let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
            u32::try_from(area.x0),
            u32::try_from(area.y0),
            u32::try_from(area.x1 - area.x0),
            u32::try_from(area.y1 - area.y0),
        ) else {
            return Outcome::none();
        };
        self.transform(
            Transform::Crop {
                x,
                y,
                width,
                height,
            },
            "crop",
            layout,
            damage,
        )
    }

    /// Invert a colour picture's pixels, or a palette picture's palette.
    fn invert(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if self.kind().palette().is_none() {
            return self.transform(Transform::Invert, "invert", layout, damage);
        }
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let Some(palette) = self.kind().palette() else {
            return Outcome::none();
        };
        let inverted: Vec<[u8; 4]> = palette
            .iter()
            .map(|&[r, g, b, a]| [255 - r, 255 - g, 255 - b, a])
            .collect();
        self.change_palette(inverted, layout, damage);
        Outcome::none()
    }

    fn duplicate(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let Entry::Picture(picture) = self.document.entry() else {
            self.state("A kept sprite cannot be copied", layout, damage);
            return Outcome::none();
        };
        // Only a sprite has a name, and the copy's must be its own.
        let name = match &picture.sprite {
            None => None,
            Some(sprite) => {
                let Some(name) = free_name(&sprite.name, self.document.names()) else {
                    self.state("No name is free for a copy", layout, damage);
                    return Outcome::none();
                };
                Some(name)
            }
        };
        let Ok(mut copy) = picture.try_clone() else {
            self.state(
                "There is not enough memory to copy the sprite",
                layout,
                damage,
            );
            return Outcome::none();
        };
        if let (Some(sprite), Some(name)) = (copy.sprite.as_mut(), name) {
            sprite.name = name;
        }
        let at = self.document.current() + 1;
        match self.document.insert(at, Entry::Picture(copy)) {
            Ok(()) => self.after_picture_change(layout, damage),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
    }

    fn delete_sprite(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let current = self.document.current();
        match self.document.remove(current) {
            Ok(()) => {
                // A layer floating over the sprite goes with it, and only then.
                self.held = None;
                self.after_picture_change(layout, damage)
            }
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
    }

    /// Escape: turn a drag under way down, else a floating layer, else
    /// forget the selection.
    fn escape(&mut self, layout: &Layout, damage: &mut Region) {
        if self.gesture.is_some() {
            self.cancel_gesture(layout, damage);
            return;
        }
        if !self.turn_down_floating(layout, damage) {
            self.drop_selection(layout, damage);
        }
    }

    /// The rows `kind` opens as.
    #[must_use]
    pub fn menu(&self, kind: MenuKind) -> AppMenu {
        let mut menu = match kind {
            MenuKind::Window => MenuBuilder::titled(APP_TITLE),
            MenuKind::Zoom => MenuBuilder::new(),
        };
        match kind {
            MenuKind::Window => self.window_rows(&mut menu),
            MenuKind::Zoom => self.zoom_rows(&mut menu, Plate::Root),
        }
        menu.finish()
    }

    fn window_rows(&self, menu: &mut MenuBuilder) {
        let selected = self.selected_area().is_some();
        let picture = self.document.picture().is_some();
        menu.item(
            Action::Cut,
            "Cut",
            "Ctrl+X",
            selected && picture,
            Plate::Root,
        );
        menu.item(
            Action::Copy,
            "Copy",
            "Ctrl+C",
            selected && picture,
            Plate::Root,
        );
        menu.item(Action::Paste, "Paste", "Ctrl+V", picture, Plate::Root);
        menu.item(
            Action::SelectAll,
            "Select all",
            "Ctrl+A",
            picture,
            Plate::Root,
        );
        menu.item(
            Action::Deselect,
            "Deselect",
            "Ctrl+D",
            selected,
            Plate::Root,
        );
        menu.separator(Plate::Root);
        if let Some(file) = menu.submenu("File", Plate::Root) {
            file_rows(menu, file);
        }
        if let Some(edit) = menu.submenu("Edit", Plate::Root) {
            self.edit_rows(menu, edit, selected);
        }
        if let Some(image) = menu.submenu("Image", Plate::Root) {
            self.image_rows(menu, image, picture);
        }
        if let Some(colours) = menu.submenu("Colours", Plate::Root) {
            menu.item(
                Action::EditPrimary,
                "Edit primary\u{2026}",
                "",
                picture,
                colours,
            );
            menu.item(
                Action::EditSecondary,
                "Edit secondary\u{2026}",
                "",
                picture,
                colours,
            );
            menu.item(Action::SwapColours, "Swap colours", "X", true, colours);
        }
        if let Some(sprites) = menu.submenu("Sprites", Plate::Root) {
            self.sprite_rows(menu, sprites);
        }
        if let Some(view) = menu.submenu("View", Plate::Root) {
            menu.item(Action::ZoomIn, "Zoom in", "+", true, view);
            menu.item(Action::ZoomOut, "Zoom out", "-", true, view);
            menu.item(Action::Fit, "Fit in window", "Ctrl+0", true, view);
            menu.item(Action::Actual, "Actual size", "1", true, view);
            menu.mark(Action::Grid, "Pixel grid", "G", self.grid, view);
        }
        if let Some(tools) = menu.submenu("Tools", Plate::Root) {
            for tool in Tool::ALL {
                menu.radio(
                    Action::Tool(tool),
                    tool.label(),
                    "",
                    tool == self.tool,
                    tools,
                );
            }
        }
    }

    fn edit_rows(&self, menu: &mut MenuBuilder, plate: Plate, selected: bool) {
        let (undo, redo) = self.document.can_undo_redo();
        menu.item(
            Action::Undo,
            "Undo",
            "Ctrl+Z",
            undo || self.held.is_some(),
            plate,
        );
        menu.item(Action::Redo, "Redo", "Ctrl+Shift+Z", redo, plate);
        menu.separator(plate);
        menu.item(Action::Delete, "Clear selection", "Delete", selected, plate);
        menu.item(
            Action::Crop,
            "Crop to selection",
            "Ctrl+Shift+X",
            selected,
            plate,
        );
        menu.item(
            Action::PutDown,
            "Put down",
            "Enter",
            self.held.is_some(),
            plate,
        );
    }

    fn image_rows(&self, menu: &mut MenuBuilder, plate: Plate, picture: bool) {
        let kind = self.kind();
        menu.item(Action::Resize, "Resize\u{2026}", "Ctrl+R", picture, plate);
        menu.item(
            Action::CanvasSize,
            "Canvas size\u{2026}",
            "Ctrl+Shift+R",
            picture,
            plate,
        );
        menu.separator(plate);
        menu.item(Action::RotateLeft, "Rotate left", "Ctrl+[", picture, plate);
        menu.item(
            Action::RotateRight,
            "Rotate right",
            "Ctrl+]",
            picture,
            plate,
        );
        menu.item(Action::RotateHalf, "Rotate half a turn", "", picture, plate);
        menu.item(Action::FlipAcross, "Flip left to right", "", picture, plate);
        menu.item(Action::FlipDown, "Flip top to bottom", "", picture, plate);
        menu.separator(plate);
        menu.item(Action::Invert, "Invert colours", "Ctrl+I", picture, plate);
        menu.item(Action::Convert, "Colours\u{2026}", "", picture, plate);
        let palette = picture && kind.palette().is_some();
        menu.item(
            Action::AddMask,
            "Add mask",
            "",
            palette && !kind.masked(),
            plate,
        );
        menu.item(
            Action::RemoveMask,
            "Remove mask",
            "",
            palette && kind.masked(),
            plate,
        );
    }

    fn sprite_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        let count = self.document.entries().len();
        let current = self.document.current();
        let many = count > 1;
        menu.item(
            Action::PreviousSprite,
            "Previous",
            "Page Up",
            current > 0,
            plate,
        );
        menu.item(
            Action::NextSprite,
            "Next",
            "Page Down",
            current + 1 < count,
            plate,
        );
        menu.entry(Action::GoTo, GO_TO_ENTRY, "Go to\u{2026}", "", plate);
        menu.separator(plate);
        menu.item(Action::NewSprite, "New sprite\u{2026}", "", true, plate);
        menu.item(
            Action::DuplicateSprite,
            "Duplicate",
            "",
            self.document.picture().is_some(),
            plate,
        );
        let name = self
            .document
            .entry()
            .name()
            .map_or_else(String::new, ToString::to_string);
        menu.entry(Action::Rename, RENAME_ENTRY, "Rename\u{2026}", &name, plate);
        menu.item(Action::DeleteSprite, "Delete", "", many, plate);
        menu.item(Action::SpriteUp, "Move up", "", current > 0, plate);
        menu.item(
            Action::SpriteDown,
            "Move down",
            "",
            current + 1 < count,
            plate,
        );
    }

    fn zoom_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        let mut label = String::new();
        for (rung, &zoom) in ZOOMS.iter().enumerate().rev() {
            label.clear();
            write_zoom(&mut label, Zoom::of(zoom).percent());
            menu.radio(
                Action::Zoom(rung),
                &label,
                "",
                self.viewport.rung() == Some(rung),
                plate,
            );
        }
    }
}

/// The File menu's rows, the same whatever the document.
fn file_rows(menu: &mut MenuBuilder, plate: Plate) {
    menu.item(
        Action::NewPicture,
        "New picture\u{2026}",
        "Ctrl+N",
        true,
        plate,
    );
    menu.item(Action::Open, "Open\u{2026}", "Ctrl+O", true, plate);
    menu.separator(plate);
    menu.item(Action::Save, "Save", "Ctrl+S", true, plate);
    menu.item(
        Action::SaveAs,
        "Save as\u{2026}",
        "Ctrl+Shift+S",
        true,
        plate,
    );
    menu.item(Action::Quality, "JPEG quality\u{2026}", "", true, plate);
    menu.separator(plate);
    menu.item(Action::Close, "Close", "Ctrl+W", true, plate);
}

/// How a modal question was answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ModalAnswer {
    /// How the close question was answered.
    Close(SaveChanges),
    /// A form's answer.
    Form(Answer),
}

/// Where the close question is drawn in `window`.
pub(crate) fn close_rect(dialog: &Dialog, window: Rect, scale: Scale, theme: &Theme) -> Rect {
    let width = scale.scale_length(Dialog::QUESTION_WIDTH);
    dialog.placed_over(window, width, 0, scale, theme)
}

/// The action a key chord asks for, whatever has the keyboard.
pub(super) fn shortcut(key: Key, modifiers: Modifiers) -> Option<Action> {
    let ctrl = modifiers.ctrl && !modifiers.alt && !modifiers.meta;
    let bare = !modifiers.ctrl && !modifiers.alt && !modifiers.meta;
    match key {
        Key::Named(NamedKey::Enter) => Some(Action::PutDown),
        Key::Named(NamedKey::Delete | NamedKey::Backspace) => Some(Action::Delete),
        Key::Named(NamedKey::PageUp) => Some(Action::PreviousSprite),
        Key::Named(NamedKey::PageDown) => Some(Action::NextSprite),
        Key::Char(ch) if ctrl => match (ch.to_ascii_lowercase(), modifiers.shift) {
            ('n', _) => Some(Action::NewPicture),
            ('o', _) => Some(Action::Open),
            ('s', true) => Some(Action::SaveAs),
            ('s', false) => Some(Action::Save),
            ('w', _) => Some(Action::Close),
            ('z', true) | ('y', _) => Some(Action::Redo),
            ('z', false) => Some(Action::Undo),
            ('x', true) => Some(Action::Crop),
            ('x', false) => Some(Action::Cut),
            ('c', _) => Some(Action::Copy),
            ('v', _) => Some(Action::Paste),
            ('a', _) => Some(Action::SelectAll),
            ('d', _) => Some(Action::Deselect),
            ('r', true) => Some(Action::CanvasSize),
            ('r', false) => Some(Action::Resize),
            ('i', _) => Some(Action::Invert),
            ('[', _) => Some(Action::RotateLeft),
            (']', _) => Some(Action::RotateRight),
            ('0', _) => Some(Action::Fit),
            ('=' | '+', _) => Some(Action::ZoomIn),
            ('-', _) => Some(Action::ZoomOut),
            _ => None,
        },
        Key::Char(ch) if bare => match ch {
            '+' | '=' => Some(Action::ZoomIn),
            '-' => Some(Action::ZoomOut),
            '1' => Some(Action::Actual),
            'g' | 'G' => Some(Action::Grid),
            'x' | 'X' => Some(Action::SwapColours),
            other => Tool::for_key(other).map(Action::Tool),
        },
        _ => None,
    }
}
