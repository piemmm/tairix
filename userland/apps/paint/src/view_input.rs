//! What the window does with input: the pointer on each part of it, keys,
//! menu rows, and the answers its workers send back.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId};
use tairix_colour::Rgba;
use tairix_controls::{
    wheel_steps, Dialog, DialogAction, Keystroke, PickerOutcome, SaveChanges, ScrollAction,
    SwatchAction, SwatchMark, ToolActivation, ToolbarOutcome,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{SpriteMode, SpriteName, SpritePalette};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PinchPhase, PointerButton};
use tairix_raster::Color;
use tairix_theme::Theme;
use tairix_util::fallible;
use tairix_window::menu::{MenuBuilder, Plate};

use super::{
    Action, Aim, Clip, Compute, Computed, Draft, Gesture, Lands, MenuKind, Modal, NewPicture,
    Outcome, Own, PaletteEdit, Pending, Request, Settles, Then, View, APP_TITLE, GO_TO_ENTRY,
    GO_TO_LAYER, RENAME_ENTRY, SPRITE_SIZE,
};
use crate::brush::{Path, Tip};
use crate::canvas::{Canvas, Kind, OutOfMemory, Sample};
use crate::colour::Ink;
use crate::dialog::{Answer, Form, Purpose, SaveChoices};
use crate::document::{free_name, Entry, Layer, Picture, SpriteInfo, NAME_REFUSAL, SPRITE_STEM};
use crate::history::{Applied, Damage, Unapplied};
use crate::layout::Layout;
use crate::mask::{Combine, Mask, Recipe};
use crate::render::write_zoom;
use crate::save::{natural, restated, writable_as, SaveFormat};
use crate::selection::Floating;
use crate::shape::{line_pixels, Bounds, Point as Fx, Shape, FX};
use crate::stroke::{Coat, Stroke};
use crate::tool::{mark_grid, tool_index, Marquee, Tool, VIEW_COMMANDS};
use crate::tool_controls::{BarOutcome, ToolControls};
use crate::transform::{Depth, Transform, TransformError, Turn};
use crate::viewport::{Zoom, ACTUAL, ZOOMS};

/// What a clearing is said not to have managed.
const CLEARING: &str = "clear that";

/// What making a selection is said not to have managed.
const SELECTING: &str = "select that";

/// How near a press must land to a polygon's first corner to close it, in
/// logical pixels.
const CLOSE_REACH: u32 = 6;

/// The least a lasso's pointer moves before its path takes another point:
/// a quarter of a pixel, past which a mask a pixel fine learns nothing.
const LASSO_STEP: i64 = FX / 4;

/// Why a selection could not be begun.
const NO_ROOM_TO_SELECT: &str = "There is not enough memory to select that";

/// Why a sprite kept as its bytes takes no edit.
const KEPT_UNCHANGED: &str = "This sprite cannot be edited; it is kept, and saved back unchanged";

impl View {
    /// Feed one pointer event.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match event {
            InputEvent::PointerMoved { to } => {
                let marker = self.clone_offset.and_then(|_| self.clone_marker(layout));
                self.pointer = *to;
                self.hovered(layout, damage);
                if marker.is_some() {
                    self.damage_marker(marker, layout, damage);
                    self.damage_marker(self.clone_marker(layout), layout, damage);
                }
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
        if self.picker.is_dragging() {
            return self
                .dock_pointer(event, layout, scale, theme, damage)
                .unwrap_or_else(Outcome::none);
        }
        // An open list owns the pointer wherever it reaches: a press on its
        // overhang is the list's, never the canvas's beneath.
        if self.bar.listing() {
            self.bar_pointer(event, layout, scale, theme, damage);
            return Outcome::none();
        }
        if matches!(event, InputEvent::PointerPressed { .. }) {
            self.release_elsewhere(layout, scale, theme, damage);
        }
        if let InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        } = event
        {
            if layout.window().contains(self.pointer) {
                // The menu takes the pointer, so the release that would have
                // ended the drag never comes here.
                self.end_gesture(layout, damage);
                self.release_keyboard(layout, scale, theme, damage);
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
            if let Some(outcome) = self.chrome_pointer(event, layout, scale, theme, damage) {
                return outcome;
            }
        }
        self.canvas_pointer(event, layout, scale, damage)
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
            None => self.filter_moved(layout, damage),
        })
    }

    /// The pointer on the window's chrome: the tool box, the view strip, the
    /// tool-controls bar, the dock, the palette, the bars and the status
    /// band. Every part sees every event, so a hover leaves and a press held
    /// on one part ends there wherever the pointer went; `None` where nothing
    /// asked for more than a repaint.
    fn chrome_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let chosen = self
            .tool_box
            .on_pointer(event, layout.tools(), scale, theme, damage);
        if let Some(&tool) = activated(chosen).and_then(|index| Tool::ALL.get(index)) {
            return Some(self.act(Action::Tool(tool), layout, damage));
        }
        let commanded = self
            .commands
            .on_pointer(event, layout.view_strip(), scale, theme, damage);
        if let Some(&(_, command, _)) =
            activated(commanded).and_then(|index| VIEW_COMMANDS.get(index))
        {
            return Some(self.act(command.into(), layout, damage));
        }
        self.bar_pointer(event, layout, scale, theme, damage);
        if let Some(outcome) = self.dock_pointer(event, layout, scale, theme, damage) {
            return Some(outcome);
        }
        if let Some(outcome) = self.palette_pointer(event, layout, damage) {
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

    /// The pointer on the tool-controls bar.
    fn bar_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> BarOutcome {
        let outcome = self.bar.on_pointer(
            event,
            self.pointer,
            layout.bar(),
            &mut self.options,
            (scale, theme),
            damage,
        );
        self.after_bar(outcome, theme, layout, damage)
    }

    /// What `outcome` of the bar means past it, however the bar was worked:
    /// a setting changed sets the text being typed again as it now stands.
    fn after_bar(
        &mut self,
        outcome: BarOutcome,
        theme: &Theme,
        layout: &Layout,
        damage: &mut Region,
    ) -> BarOutcome {
        if outcome == BarOutcome::Changed && self.text.is_some() {
            self.reset_text(theme, layout, damage);
        }
        outcome
    }

    /// The pointer on the palette strip: a well chosen by the primary button
    /// is the primary ink, and one pressed with the middle button the
    /// secondary.
    fn palette_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let middle = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Middle
            }
        );
        if middle {
            if let Some(index) = self.swatches.well_at(layout.swatches(), self.pointer) {
                self.choose_well(SwatchMark::Secondary, index, layout, damage);
                return Some(Outcome::none());
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
            None => None,
        }
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
        self.inks_changed(layout, damage);
    }

    /// The inks changed from outside the dock: show them in the wells, the
    /// palette's marks and the picker.
    fn inks_changed(&mut self, layout: &Layout, damage: &mut Region) {
        self.mark_wells();
        self.sync_picker();
        damage.add(layout.wells());
        damage.add(layout.swatches());
        damage.add(layout.picker());
    }

    /// The pointer on the colour dock: a press on a well makes it the one the
    /// picker edits, and the picker takes the rest. `None` where the event is
    /// none of the dock's.
    fn dock_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let pressed = matches!(event, InputEvent::PointerPressed { .. });
        if pressed && !self.picker.is_dragging() {
            for (well, mark) in [
                (layout.primary_well(), SwatchMark::Primary),
                (layout.secondary_well(), SwatchMark::Secondary),
            ] {
                if well.contains(self.pointer) {
                    self.edit_well(mark, layout, scale, theme, damage);
                    return Some(Outcome::none());
                }
            }
        }
        let outcome = self
            .picker
            .on_pointer(event, layout.picker(), scale, theme, damage);
        if pressed && outcome != PickerOutcome::Ignored && !self.picker.state().focus.focused {
            self.picker.set_focused(true);
            damage.add(layout.picker());
        }
        match outcome {
            PickerOutcome::Ignored => {
                (pressed && layout.dock().contains(self.pointer)).then(Outcome::none)
            }
            outcome => Some(self.picked(outcome, layout, damage)),
        }
    }

    /// Make `mark`'s ink the one the dock edits, finishing what the picker
    /// held for the other.
    fn edit_well(
        &mut self,
        mark: SwatchMark,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if mark == self.editing {
            return;
        }
        self.commit_dock(layout, scale, theme, damage);
        self.editing = mark;
        self.sync_picker();
        damage.add(layout.wells());
        damage.add(layout.picker());
    }

    /// Settle what the picker holds — a drag, a field's typing — keeping its
    /// keyboard focus: done before anything else touches the inks or the
    /// picture.
    pub(super) fn commit_dock(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let dragged = self.picker.finish_drag();
        self.picked(dragged, layout, damage);
        let committed = self.picker.commit(layout.picker(), scale, theme, damage);
        self.picked(committed, layout, damage);
    }

    /// Settle the picker and take the keyboard from it, as a press elsewhere
    /// in the window or a menu does.
    pub(super) fn release_dock(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.commit_dock(layout, scale, theme, damage);
        if self.picker.state().focus.focused {
            let blurred = self.picker.blur(layout.picker(), scale, theme, damage);
            self.picked(blurred, layout, damage);
        }
    }

    /// Settle the tool-controls bar's typing, keeping its keyboard focus.
    pub(super) fn commit_bar(&mut self, layout: &Layout, damage: &mut Region) {
        self.bar.commit(layout.bar(), &mut self.options, damage);
    }

    /// Settle the bar and take the keyboard from it.
    fn release_bar(&mut self, layout: &Layout, damage: &mut Region) {
        self.bar.blur(layout.bar(), &mut self.options, damage);
    }

    /// Take the keyboard from the palette strip.
    fn release_palette(&mut self, layout: &Layout, damage: &mut Region) {
        if self.swatches.state().focus.focused {
            self.swatches.set_focused(false);
            damage.add(layout.swatches());
        }
    }

    /// Settle every part that holds the keyboard and give it back to the
    /// picture, as a menu or Escape does.
    fn release_keyboard(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.release_dock(layout, scale, theme, damage);
        self.release_bar(layout, damage);
        self.release_palette(layout, damage);
    }

    /// A press settles and takes the keyboard from every part it did not land
    /// on: the bar and the dock keep it only for a press on themselves, and
    /// the palette, which takes it only from Tab, gives it up to any press.
    fn release_elsewhere(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if !layout.dock().contains(self.pointer) {
            self.release_dock(layout, scale, theme, damage);
        }
        if !layout.controls().contains(self.pointer) {
            self.release_bar(layout, damage);
        }
        self.release_palette(layout, damage);
    }

    /// Which part of the window has the keyboard.
    fn keyboard(&self) -> Keyboard {
        if self.picker.state().focus.focused || self.picker.is_dragging() {
            Keyboard::Dock
        } else if self.bar.focus().is_some() {
            Keyboard::Bar
        } else if self.swatches.state().focus.focused {
            Keyboard::Palette
        } else {
            Keyboard::Picture
        }
    }

    /// Carry the keyboard on from `from` to the next part that takes it, in
    /// the order Tab walks — the picture, the bar, the palette, the dock —
    /// or back the other way; the picture always takes it.
    fn walk_keyboard(
        &mut self,
        from: Keyboard,
        forward: bool,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.release_keyboard(layout, scale, theme, damage);
        let order = Keyboard::ORDER;
        let mut at = order.iter().position(|&part| part == from).unwrap_or(0);
        loop {
            at = if forward {
                (at + 1) % order.len()
            } else {
                (at + order.len() - 1) % order.len()
            };
            if self.enter(order[at], forward, layout, scale, theme, damage) {
                return;
            }
        }
    }

    /// Give `part` the keyboard, at its first stop or its last when not
    /// `forward`: `false` where it has none to take it.
    fn enter(
        &mut self,
        part: Keyboard,
        forward: bool,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        match part {
            Keyboard::Picture => true,
            Keyboard::Bar => self.bar.enter_focus(forward, layout.bar(), damage),
            Keyboard::Palette => {
                if self.swatches.is_empty() || layout.swatches().is_empty() {
                    return false;
                }
                self.swatches.set_focused(true);
                damage.add(layout.swatches());
                true
            }
            Keyboard::Dock => {
                if layout.picker().is_empty() || !self.picker.state().is_actionable() {
                    return false;
                }
                self.picker
                    .enter_focus(forward, layout.picker(), scale, theme);
                damage.add(layout.picker());
                true
            }
        }
    }

    /// What the picker concluded, landed on the ink it edits: a colour
    /// picture's ink takes the colour, and a palette picture's ink names an
    /// entry, which takes it live and is recorded once it settles.
    fn picked(&mut self, outcome: PickerOutcome, layout: &Layout, damage: &mut Region) -> Outcome {
        let (colour, settled) = match outcome {
            PickerOutcome::Edited(colour) => (colour, false),
            PickerOutcome::Settled(colour) => (colour, true),
            PickerOutcome::Taken | PickerOutcome::Ignored => return Outcome::none(),
        };
        let ink = self.ink(self.editing == SwatchMark::Secondary);
        match (self.kind(), ink) {
            (Kind::Rgba, _) => {
                let ink = Ink::of_colour(colour.to_array());
                match self.editing {
                    SwatchMark::Primary => self.primary = ink,
                    SwatchMark::Secondary => self.secondary = ink,
                }
                self.mark_wells();
                damage.add(layout.wells());
                damage.add(layout.swatches());
            }
            (Kind::Indexed { .. }, Ink::Index(entry)) => {
                self.edit_entry(entry, colour, settled, layout, damage);
            }
            (Kind::Indexed { .. }, _) => {}
        }
        Outcome::none()
    }

    /// Give palette entry `entry` the colour `colour` live, the palette it
    /// had kept aside so that settling records the change as one step.
    fn edit_entry(
        &mut self,
        entry: u8,
        colour: Rgba,
        settled: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        if self
            .palette_edit
            .as_ref()
            .is_some_and(|edit| edit.entry != entry)
        {
            self.settle_entry_edit(layout, damage);
        }
        if self.palette_edit.is_none() && !self.begin_entry_edit(entry, layout, damage) {
            self.sync_picker();
            damage.add(layout.picker());
            return;
        }
        // A sprite's palette holds colours; its transparency is its mask.
        let sprite = self
            .document
            .picture()
            .is_some_and(|picture| picture.sprite.is_some());
        let colour = if sprite {
            colour.with_alpha(u8::MAX)
        } else {
            colour
        };
        if let Some(canvas) = self.document.canvas_mut() {
            canvas.set_palette_entry(entry, colour.to_array());
        }
        if let Some(well) = self.wells.iter().position(|&ink| ink == Ink::Index(entry)) {
            self.swatches.set_colour(well, Color::from(colour));
            if let Some(cell) = self.swatches.cell_rect(layout.swatches(), well) {
                damage.add(cell);
            }
        }
        damage.add(layout.canvas());
        damage.add(layout.wells());
        if settled {
            self.settle_entry_edit(layout, damage);
        }
    }

    /// Keep the palette as it stands aside and the room to record its change,
    /// before an entry is edited live; `false`, saying why, where it may not
    /// be.
    fn begin_entry_edit(&mut self, entry: u8, layout: &Layout, damage: &mut Region) -> bool {
        if !self.editable(layout, damage) {
            return false;
        }
        let length = self.kind().palette().map_or(0, <[_]>::len);
        let mut before = Vec::new();
        if before.try_reserve_exact(length).is_err() || self.document.reserve().is_err() {
            self.state(
                "There is not enough memory to change the palette",
                layout,
                damage,
            );
            return false;
        }
        let Some(palette) = self.kind().palette() else {
            return false;
        };
        before.extend_from_slice(palette);
        self.palette_edit = Some(PaletteEdit { entry, before });
        true
    }

    /// End a live entry edit: the palette it began from is put back and the
    /// edited one made the picture's as one step, or nothing recorded where
    /// it came back to where it began.
    fn settle_entry_edit(&mut self, layout: &Layout, damage: &mut Region) {
        let Some(PaletteEdit { before, .. }) = self.palette_edit.take() else {
            return;
        };
        let Some(edited) = self
            .document
            .canvas_mut()
            .and_then(|canvas| canvas.swap_palette(before))
        else {
            return;
        };
        if self.kind().palette() == Some(edited.as_slice()) {
            return;
        }
        self.change_palette(edited, layout, damage);
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
            self.tool_box
                .wheel(dx, dy, layout.tools(), scale, theme, damage);
            return Outcome::none();
        }
        if layout.view_strip().contains(self.pointer) {
            self.commands
                .wheel(dx, dy, layout.view_strip(), scale, theme, damage);
            return Outcome::none();
        }
        if layout.controls().contains(self.pointer) {
            let turned = InputEvent::PointerScrolled { dx, dy };
            self.bar_pointer(&turned, layout, scale, theme, damage);
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

    /// Choose `tool`: a floating selection was put down before this was reached,
    /// and the selection stays, holding what the tool paints.
    fn choose_tool(&mut self, tool: Tool, layout: &Layout, damage: &mut Region) -> Outcome {
        if tool == self.tool {
            return Outcome::none();
        }
        // An open list hanging over the canvas goes with the bar it hung
        // from, so only then is more than the bar and the tool box drawn.
        let listing = self.bar.listing();
        self.commit_bar(layout, damage);
        self.tool = tool;
        self.tool_box.set_active(tool_index(tool));
        self.bar = ToolControls::new(tool, self.options, self.kind().sample_bytes() == 4);
        if listing {
            return Outcome::relaid();
        }
        damage.add(layout.top());
        damage.add(layout.tool_box());
        Outcome::reshaped()
    }

    /// The pointer on the canvas.
    fn canvas_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
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
                let outcome = self.press(secondary, layout, scale, damage);
                self.dragging = self.gesture.is_some().then_some(*button);
                outcome
            }
            InputEvent::PointerMoved { .. } => {
                self.drag(layout, scale, damage);
                Outcome::none()
            }
            InputEvent::PointerReleased { button } if self.dragging == Some(*button) => {
                self.release(layout, damage)
            }
            _ => Outcome::none(),
        }
    }

    /// Where the pointer is on the picture, in picture units.
    pub(super) fn at(&self, layout: &Layout) -> Fx {
        let (x, y) = self
            .viewport
            .to_picture(self.pointer, self.picture_size(), layout.canvas());
        Fx { x, y }
    }

    /// Refuse a change to the document, or to the entry showing, while a
    /// worker has the picture, saying why: its answer is written over the
    /// state it was asked of.
    pub(super) fn idle(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if self.pending.is_none() {
            return true;
        }
        self.state("Wait: the picture is being worked on", layout, damage);
        false
    }

    /// Refuse an edit while the picture waits on a worker or is not a
    /// picture at all, saying why.
    pub(super) fn editable(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if !self.idle(layout, damage) {
            return false;
        }
        if self.document.picture().is_none() {
            self.state(KEPT_UNCHANGED, layout, damage);
            return false;
        }
        true
    }

    pub(super) fn state(&mut self, message: &str, layout: &Layout, damage: &mut Region) {
        self.message = Some(String::from(message));
        damage.add(layout.message());
    }

    /// A press on the canvas: begin what the tool does. Alt takes a colour
    /// with any tool but the select tool, whose Alt takes away from the
    /// selection.
    fn press(
        &mut self,
        secondary: bool,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        // The view's own tools, and Space held, act whatever the picture is
        // doing.
        if self.panning() {
            self.begin_pan();
            return Outcome::none();
        }
        if self.tool == Tool::Zoom {
            self.begin_zoom_box();
            return Outcome::none();
        }
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let at = self.at(layout);
        let own_alt = matches!(self.tool, Tool::Select | Tool::Clone);
        if self.tool == Tool::Eyedropper || self.modifiers.alt && !own_alt {
            self.pick(at, secondary, layout, damage);
            return Outcome::none();
        }
        match self.tool {
            Tool::Select => self.select_press(at, layout, scale, damage),
            Tool::Fill => self.fill_at(at, secondary, layout, damage),
            Tool::Gradient => {
                self.begin_gradient(at, secondary);
                Outcome::none()
            }
            Tool::Clone => {
                self.clone_press(at, layout, damage);
                Outcome::none()
            }
            Tool::Text => {
                self.text_press(at, layout, damage);
                Outcome::none()
            }
            Tool::Polygon => self.polygon_press(at, secondary, layout, scale, damage),
            Tool::Crop => {
                self.crop_press(at, layout, scale, damage);
                Outcome::none()
            }
            Tool::Hand | Tool::Zoom | Tool::Eyedropper => Outcome::none(),
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
    pub(super) fn reserve(&mut self, layout: &Layout, damage: &mut Region) -> bool {
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
        let Some(sample) = picture.canvas().sample(x, y) else {
            return;
        };
        // A picture of layers gives the colour they show together there.
        let kind = picture.kind();
        let ink = if picture.single() {
            Ink::of_sample(sample, kind)
        } else {
            Ink::of_colour(picture.shown_at((x, y), kind.colour(sample)))
        };
        if secondary {
            self.secondary = ink;
        } else {
            self.primary = ink;
        }
        self.inks_changed(layout, damage);
    }

    pub(super) fn begin_stroke(
        &mut self,
        at: Fx,
        secondary: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        if !self.reserve(layout, damage) {
            return;
        }
        let kind = self.kind();
        let ink = match self.tool {
            Tool::Eraser => self.eraser_ink(kind),
            _ => self.ink(secondary),
        };
        let clip = self.selection.clone();
        // A tip lays its paint over what is there; a pencil sets pixels.
        let over = Self::coat(ink, kind, true);
        let stroke = match (self.tip(kind), self.clone_offset) {
            (Some(tip), Some(offset)) if self.tool == Tool::Clone => {
                Stroke::cloning(offset, over.blend, tip.opacity_255(), clip)
            }
            (Some(_), None) if self.tool == Tool::Clone => return,
            (Some(tip), _) => Stroke::building(over, tip.opacity_255(), clip),
            (None, _) => Stroke::new(Self::coat(ink, kind, false), None, clip),
        };
        // What was said before this stroke is done with; what it says itself
        // stays once it ends.
        if self.message.take().is_some() {
            damage.add(layout.message());
        }
        self.gesture = Some(Gesture::Stroke {
            stroke: Box::new(stroke),
            path: Path::new(at),
        });
        self.lay(
            |stroke, canvas, path, tip, aa| {
                if let Some(tip) = tip {
                    tip.dab(stroke, canvas, path.last(), aa)
                } else {
                    let (x, y) = path.last().pixel();
                    stroke.cover_pixel(canvas, 0, x, y)
                }
            },
            layout,
            damage,
        );
    }

    /// The tip the tool in use paints with on `kind`, if it has one: whole on
    /// a palette picture, which cannot show part of a pixel.
    pub(super) fn tip(&self, kind: &Kind) -> Option<Tip> {
        let tip = *self.options.tip(self.tool)?;
        Some(if kind.sample_bytes() == 4 {
            tip
        } else {
            tip.whole()
        })
    }

    /// Lay more of the stroke under way through `paint`, handed the stroke,
    /// the picture, its path, the tip and whether edges are smoothed, then
    /// report what it changed.
    fn lay(
        &mut self,
        paint: impl FnOnce(
            &mut Stroke,
            &mut Canvas,
            &mut Path,
            Option<Tip>,
            bool,
        ) -> Result<(), OutOfMemory>,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let kind = self.kind();
        let (tip, aa) = (self.tip(kind), self.smooth(kind));
        let Some(Gesture::Stroke { stroke, path }) = &mut self.gesture else {
            return;
        };
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let written = paint(stroke, canvas, path, tip, aa);
        let bounds = stroke.take_damage();
        self.damage_picture(bounds, layout, damage);
        if written.is_err() {
            self.state("There is not enough memory to draw more", layout, damage);
        }
    }

    /// Carry the stroke under way on to `to`: dabs a spacing apart, or a
    /// pencil's every pixel.
    fn stroke_to(&mut self, to: Fx, layout: &Layout, damage: &mut Region) {
        self.lay(
            |stroke, canvas, path, tip, aa| {
                if let Some(tip) = tip {
                    return path.to(to, tip.step(), |at| tip.dab(stroke, canvas, at, aa));
                }
                let from = path.move_to(to);
                let mut outcome = Ok(());
                line_pixels(from.pixel(), to.pixel(), |x, y| {
                    if outcome.is_ok() {
                        outcome = stroke.cover_pixel(canvas, 0, x, y);
                    }
                });
                outcome
            },
            layout,
            damage,
        );
    }

    /// Lay one more dab where the airbrush is held: paint building up the
    /// longer it stays.
    pub(crate) fn airbrush(&mut self, layout: &Layout, damage: &mut Region) {
        self.lay(
            |stroke, canvas, path, tip, aa| {
                tip.map_or(Ok(()), |tip| tip.dab(stroke, canvas, path.last(), aa))
            },
            layout,
            damage,
        );
    }

    /// The pointer moved with a drag under way.
    fn drag(&mut self, layout: &Layout, scale: Scale, damage: &mut Region) {
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
            Some(Gesture::Marquee { from, to, combine }) => {
                let before = self.marquee_shape(from, to).bounds();
                self.gesture = Some(Gesture::Marquee {
                    from,
                    to: at,
                    combine,
                });
                if at.pixel() != to.pixel() {
                    self.damage_picture(Some(before), layout, damage);
                    let after = self.marquee_shape(from, at).bounds();
                    self.damage_picture(Some(after), layout, damage);
                }
            }
            Some(Gesture::Lasso { .. }) => self.lasso_to(at, layout, damage),
            Some(Gesture::Move { from, last }) => {
                let now = at.pixel();
                self.gesture = Some(Gesture::Move { from, last: now });
                self.shift_floating((now.0 - last.0, now.1 - last.1), layout, damage);
            }
            Some(Gesture::Gradient {
                from, secondary, ..
            }) => {
                self.gesture = Some(Gesture::Gradient {
                    from,
                    to: at,
                    secondary,
                });
                damage.add(layout.canvas());
            }
            Some(Gesture::Pan { from, scroll }) => self.pan_to((from, scroll), layout, damage),
            Some(Gesture::ZoomBox { .. }) => self.zoom_box_to(layout, damage),
            Some(Gesture::CropNew { .. } | Gesture::CropAdjust { .. }) => {
                self.crop_to(at.pixel(), layout, scale, damage);
            }
            None => self.follow_draft(at, layout, damage),
        }
    }

    /// Carry the lasso's path on to `at`.
    fn lasso_to(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        let Some(Gesture::Lasso { points, .. }) = &mut self.gesture else {
            return;
        };
        let Some(&last) = points.last() else {
            return;
        };
        let near = (at.x - last.x).abs() < LASSO_STEP && (at.y - last.y).abs() < LASSO_STEP;
        // A path that cannot grow keeps the outline it has.
        if near || !fallible::reserve(points, 1) {
            return;
        }
        points.push(at);
        self.damage_picture(Some(segment(last, at)), layout, damage);
    }

    /// The pointer moved with a polygon being marked out: its next edge
    /// follows.
    fn follow_draft(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        let Some(&last) = draft.corners.last() else {
            return;
        };
        let before = core::mem::replace(&mut draft.to, at);
        if before.pixel() != at.pixel() {
            self.damage_picture(Some(segment(last, before)), layout, damage);
            self.damage_picture(Some(segment(last, at)), layout, damage);
        }
    }

    /// Shift the floating selection by `(dx, dy)` pixels.
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
            .coats
            .iter()
            .flatten()
            .map(|(_, shape)| shape.bounds())
            .reduce(|a, b| a.union(&b))
    }

    /// The drag's own button let go: finish the drag, marking out the
    /// selection a marquee or a lasso drew. A click that drew nothing marks
    /// nothing.
    fn release(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        match self.gesture.take() {
            Some(Gesture::Marquee { from, to, combine }) => {
                self.dragging = None;
                let shape = self.marquee_shape(from, to);
                self.damage_picture(Some(shape.bounds()), layout, damage);
                if from == to {
                    return Outcome::none();
                }
                self.select(Recipe::Shape(shape), combine, layout, damage)
            }
            Some(Gesture::Lasso { points, combine }) => {
                self.dragging = None;
                self.damage_picture(path_bounds(&points, None), layout, damage);
                if points.len() < 3 {
                    return Outcome::none();
                }
                self.select(Recipe::Outline(points), combine, layout, damage)
            }
            Some(gesture @ Gesture::Gradient { .. }) => {
                self.gesture = Some(gesture);
                let gradient = self.gradient();
                self.gesture = None;
                self.dragging = None;
                match gradient {
                    Some(gradient) => self.gradient_done(gradient, layout, damage),
                    None => Outcome::none(),
                }
            }
            Some(Gesture::ZoomBox { from, to, out }) => {
                self.dragging = None;
                self.zoom_box_done((from, to, out), layout, damage);
                Outcome::none()
            }
            Some(Gesture::CropNew { from, .. }) => {
                self.dragging = None;
                // A click sets nothing out: it lets the box go.
                let clicked = Bounds {
                    x0: from.0,
                    y0: from.1,
                    x1: from.0 + 1,
                    y1: from.1 + 1,
                };
                if self.crop.is_none_or(|crop| crop == clicked) {
                    self.crop = None;
                    damage.add(layout.canvas());
                }
                Outcome::none()
            }
            gesture => {
                self.gesture = gesture;
                self.end_gesture(layout, damage);
                Outcome::none()
            }
        }
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
        let [Some((first, first_shape)), second] = self.shape_coats(from, to, secondary, kind)
        else {
            return;
        };
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let mut stroke = Stroke::new(
            first,
            second.map(|(layer, _)| layer),
            self.selection.clone(),
        );
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
        if picture.canvas().sample(x, y).is_none() {
            return Outcome::none();
        }
        let ink = self.ink(secondary);
        let coat = Coat {
            ink,
            blend: if ink == Ink::Clear {
                crate::stroke::Blend::Replace
            } else {
                crate::stroke::Blend::Over
            },
        };
        let Ok(canvas) = picture.canvas().try_clone() else {
            self.state("There is not enough memory to fill", layout, damage);
            return Outcome::none();
        };
        let tolerance = self.options.tolerance;
        self.begin_work(
            Compute::Fill {
                canvas,
                at: (x, y),
                tolerance,
                coat,
                contiguous: self.options.contiguous,
                clip: self.selection.clone(),
            },
            Lands::Tiles(Settles::Nothing),
            "fill",
            layout,
            damage,
        )
    }

    /// Hand `work` to a worker; the picture takes no edits until it is back,
    /// and `lands` says what its answer then becomes.
    pub(super) fn begin_work(
        &mut self,
        work: Compute,
        lands: Lands,
        what: &'static str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let job = self.next_job;
        self.next_job += 1;
        self.pending = Some(Pending {
            job,
            entry: self.document.current(),
            layer: self.active_layer(),
            generation: self.document.generation(),
            lands,
            what,
        });
        self.sync_picker();
        damage.add(layout.picker());
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
        if self.previewing(job) {
            return self.previewed(answer, layout, damage);
        }
        let Some(pending) = self.pending.take_if(|pending| pending.job == job) else {
            return Outcome::none();
        };
        self.sync_picker();
        damage.add(layout.picker());
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
        self.land(pending, answer, layout, damage)
    }

    /// Land a worker's answer to `pending` on the picture it was asked of.
    fn land(
        &mut self,
        pending: Pending,
        answer: Computed,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let what = pending.what;
        match (answer, pending.lands) {
            (Computed::Filtered(Ok(tiles)) | Computed::Tiles(Ok(tiles)), Lands::Tiles(settles)) => {
                let filled = self.document.picture().and_then(|picture| {
                    tiles
                        .iter()
                        .map(|(index, _)| picture.canvas().tile_rect(*index).bounds())
                        .reduce(|a, b| a.union(&b))
                });
                let refusal = match self.document.adopt_tiles(pending.layer, tiles) {
                    Ok(true) => {
                        self.damage_picture(filled, layout, damage);
                        return self.settle_selection(settles, layout, damage);
                    }
                    Ok(false) => alloc::format!(
                        "What was asked no longer fits the picture, so it could not {what}"
                    ),
                    Err(OutOfMemory) => alloc::format!("There is not enough memory to {what}"),
                };
                self.say(refusal);
            }
            (Computed::Filtered(Err(crate::filter::FilterError::NeedsColour)), _) => {
                self.say(alloc::format!("This picture cannot {what}"));
            }
            (
                Computed::Filtered(Err(crate::filter::FilterError::OutOfMemory))
                | Computed::Tiles(Err(OutOfMemory))
                | Computed::Selection(Err(OutOfMemory))
                | Computed::Composed(Err(OutOfMemory))
                | Computed::Survey(Err(OutOfMemory)),
                _,
            ) => self.say(alloc::format!("There is not enough memory to {what}")),
            (Computed::Selection(Ok(marked)), Lands::Selection) => {
                self.adopt_selection(marked, layout, damage);
            }
            (Computed::Picture(Ok(canvases)), Lands::Transform(transform)) => {
                return self.adopt_transformed(canvases, transform, layout, damage);
            }
            (Computed::Composed(Ok(canvas)), Lands::Merged(range)) => {
                return self.adopt_merged(canvas, range, layout, damage);
            }
            (Computed::Survey(Ok(formats)), Lands::Sheet { then_close }) => {
                self.put_up_save_as(formats, then_close, layout, damage);
            }
            (Computed::Picture(Err(err)), _) => {
                let reason = match err {
                    TransformError::OutOfMemory => {
                        alloc::format!("There is not enough memory to {what}")
                    }
                    TransformError::BadSize => {
                        alloc::format!("The picture cannot {what}: it would be too large or empty")
                    }
                    TransformError::NotApplicable => alloc::format!("This picture cannot {what}"),
                };
                self.say(reason);
            }
            // Every job lands as what it was asked for.
            _ => self.say(alloc::format!("The picture could not {what}")),
        }
        Outcome::none()
    }

    /// A transform's layers are in: they replace the picture's, its sprite
    /// details refitted to what it became.
    fn adopt_transformed(
        &mut self,
        canvases: Vec<Canvas>,
        transform: Transform,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let made = self.document.picture().and_then(|held| {
            let mut picture = held.with_canvases(canvases)?;
            // A sprite is one layer.
            picture.sprite = picture
                .sprite
                .as_ref()
                .map(|sprite| sprite.refit(transform, picture.canvas()));
            Some(picture)
        });
        match made.map(|picture| self.document.replace_picture(picture)) {
            Some(Ok(true)) => {}
            Some(Ok(false)) | None => self.say(KEPT_UNCHANGED),
            Some(Err(OutOfMemory)) => self.say("There is not enough memory to keep the change"),
        }
        self.after_picture_change(layout, damage)
    }

    /// Layers `range`, laid together as `canvas`, are in: they become one
    /// layer, shown wholly under the lowest one's name, and it is painted on.
    fn adopt_merged(
        &mut self,
        canvas: Canvas,
        range: core::ops::Range<usize>,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let merged = self.document.picture().and_then(|held| {
            let mut name = String::new();
            let lowest = &held.layers().get(range.start)?.name;
            name.try_reserve_exact(lowest.len()).ok()?;
            name.push_str(lowest);
            let mut one = Some(Layer::new(canvas, name));
            let mut layers = Vec::new();
            layers
                .try_reserve_exact(held.layers().len() + 1 - range.len())
                .ok()?;
            for (index, layer) in held.layers().iter().enumerate() {
                if index == range.start {
                    layers.push(one.take()?);
                } else if !range.contains(&index) {
                    layers.push(layer.try_clone().ok()?);
                }
            }
            held.with_layers(layers, range.start)
        });
        match merged.map(|picture| self.document.replace_picture(picture)) {
            Some(Ok(true)) => {}
            Some(Ok(false)) | None => self.say("There is not enough memory to merge the layers"),
            Some(Err(OutOfMemory)) => self.say("There is not enough memory to keep the change"),
        }
        Self::layers_changed(layout, damage)
    }

    /// The layers changed, the picture's size and kind kept: what shows, and
    /// what the status band says of them, is drawn again.
    pub(super) fn layers_changed(layout: &Layout, damage: &mut Region) -> Outcome {
        damage.add(layout.canvas());
        damage.add(layout.status());
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
                    self.selection = held
                        .selection()
                        .and_then(|chosen| chosen.within(Bounds::picture(width, height)));
                    self.damage_picture(Some(held.bounds()), layout, damage);
                }
                self.carry_on(then, layout, damage)
            }
        }
    }

    /// The picture showing was replaced, or another shown: carry the inks,
    /// the palette, the bar and the view over to it.
    fn after_picture_change(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        self.selection = None;
        self.draft = None;
        self.crop = None;
        self.adopt_kind();
        damage.add(layout.window());
        self.settle(layout, damage);
        Outcome::relaid()
    }

    /// A press with the select tool: place a polygon's next corner, drag
    /// the floating selection, put it down, lift the selection, or begin
    /// marking a selection out — which, met with the one held, may begin
    /// inside it.
    fn select_press(
        &mut self,
        at: Fx,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        if self.draft.is_some() {
            return self.place_corner(at, layout, scale, damage);
        }
        let pixel = at.pixel();
        if self
            .floating()
            .is_some_and(|floating| floating.chooses(pixel.0, pixel.1))
        {
            self.gesture = Some(Gesture::Move {
                from: pixel,
                last: pixel,
            });
            return Outcome::none();
        }
        if self.held.is_some() {
            // A press off the layer puts it down; the next marks anew.
            return self.put_down_then(Then::Rest, layout, damage);
        }
        let combine = self.combine();
        let inside = self
            .selection
            .as_ref()
            .is_some_and(|chosen| chosen.chooses(pixel.0, pixel.1));
        if combine == Combine::Replace {
            if inside {
                if self.lift(layout, damage) {
                    self.gesture = Some(Gesture::Move {
                        from: pixel,
                        last: pixel,
                    });
                }
                return Outcome::none();
            }
            self.drop_selection(layout, damage);
        }
        match self.options.marquee {
            Marquee::Rectangle | Marquee::Ellipse => {
                self.gesture = Some(Gesture::Marquee {
                    from: at,
                    to: at,
                    combine,
                });
            }
            Marquee::Lasso => match fallible::collected(1, core::iter::once(at)) {
                Some(points) => self.gesture = Some(Gesture::Lasso { points, combine }),
                None => self.state(NO_ROOM_TO_SELECT, layout, damage),
            },
            Marquee::Polygon => match fallible::collected(1, core::iter::once(at)) {
                Some(corners) => {
                    self.draft = Some(Draft {
                        corners,
                        to: at,
                        aim: Aim::Select(combine),
                    });
                    self.damage_picture(Some(segment(at, at)), layout, damage);
                }
                None => self.state(NO_ROOM_TO_SELECT, layout, damage),
            },
            Marquee::Wand => return self.wand(pixel, combine, layout, damage),
        }
        Outcome::none()
    }

    /// How a selection marked out now meets the one held: Shift adds, Alt
    /// takes away, both keep only what both choose, and otherwise the bar's
    /// setting says.
    fn combine(&self) -> Combine {
        match (self.modifiers.shift, self.modifiers.alt) {
            (true, true) => Combine::Intersect,
            (true, false) => Combine::Add,
            (false, true) => Combine::Subtract,
            (false, false) => self.options.combine,
        }
    }

    /// Choose the pixels joined to pixel `at` through colours like it.
    fn wand(
        &mut self,
        (x, y): (i64, i64),
        combine: Combine,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return Outcome::none();
        };
        let Some(picture) = self.document.picture() else {
            return Outcome::none();
        };
        if picture.canvas().sample(x, y).is_none() {
            return Outcome::none();
        }
        let Ok(canvas) = picture.canvas().try_clone() else {
            self.state(NO_ROOM_TO_SELECT, layout, damage);
            return Outcome::none();
        };
        let recipe = Recipe::Wand {
            canvas,
            at: (x, y),
            tolerance: self.options.tolerance,
        };
        self.select(recipe, combine, layout, damage)
    }

    /// Make the selection `recipe` describes and meet it with the one held
    /// as `combine` says: a plain rectangle at once, anything whose cost
    /// grows with the picture on a worker.
    fn select(
        &mut self,
        recipe: Recipe,
        combine: Combine,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let (width, height) = self.picture_size();
        let within = Bounds::picture(width, height);
        let feather = self.options.feather;
        let before = (combine != Combine::Replace)
            .then(|| self.selection.clone())
            .flatten();
        if let Recipe::Shape(Shape::Rect {
            span,
            outline: None,
        }) = &recipe
        {
            let alone = combine == Combine::Replace || combine == Combine::Add && before.is_none();
            if feather == 0 && alone {
                let marked = Mask::rect(span.bounds().intersection(&within));
                self.adopt_selection(marked, layout, damage);
                return Outcome::none();
            }
        }
        let smooth = self.smooth(self.kind());
        let work = Compute::Select {
            recipe,
            before,
            combine,
            edge: (feather, smooth),
            within,
        };
        self.begin_work(work, Lands::Selection, SELECTING, layout, damage)
    }

    /// Hold `marked` as the selection.
    fn adopt_selection(&mut self, marked: Option<Mask>, layout: &Layout, damage: &mut Region) {
        let before = core::mem::replace(&mut self.selection, marked);
        self.damage_picture(before.as_ref().map(Mask::bounds), layout, damage);
        self.damage_picture(self.selection.as_ref().map(Mask::bounds), layout, damage);
    }

    /// A press placing a polygon's next corner at `at`, or closing it where
    /// the press lands on its first corner.
    pub(super) fn place_corner(
        &mut self,
        at: Fx,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        let reach = i64::from(scale.scale_length(CLOSE_REACH));
        let (size, area, pointer) = (self.picture_size(), layout.canvas(), self.pointer);
        let Some(draft) = &mut self.draft else {
            return Outcome::none();
        };
        let closes = draft.corners.len() >= 3
            && draft.corners.first().is_some_and(|first| {
                let (x, y) = self.viewport.screen_of((first.x, first.y), size, area);
                (x - i64::from(pointer.x)).abs() <= reach
                    && (y - i64::from(pointer.y)).abs() <= reach
            });
        if closes {
            return self.close_draft(layout, damage);
        }
        let Some(&last) = draft.corners.last() else {
            return Outcome::none();
        };
        if last == at {
            return Outcome::none();
        }
        if !fallible::reserve(&mut draft.corners, 1) {
            self.state(
                "There is not enough memory for another corner",
                layout,
                damage,
            );
            return Outcome::none();
        }
        draft.corners.push(at);
        self.damage_picture(Some(segment(last, at)), layout, damage);
        Outcome::none()
    }

    /// Take the polygon's last corner back; with none left it is gone.
    fn unplace_corner(&mut self, layout: &Layout, damage: &mut Region) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        let bounds = path_bounds(&draft.corners, Some(draft.to));
        draft.corners.pop();
        if draft.corners.is_empty() {
            self.draft = None;
        }
        self.damage_picture(bounds, layout, damage);
    }

    /// Close the polygon being marked out and make the selection it
    /// encloses; one of fewer than three corners encloses nothing.
    fn close_draft(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(draft) = self.draft.take() else {
            return Outcome::none();
        };
        self.damage_picture(path_bounds(&draft.corners, Some(draft.to)), layout, damage);
        if draft.corners.len() < 3 {
            return Outcome::none();
        }
        match draft.aim {
            Aim::Select(combine) => {
                self.select(Recipe::Outline(draft.corners), combine, layout, damage)
            }
            Aim::Shape { secondary } => {
                self.put_polygon(&draft.corners, secondary, layout, damage);
                Outcome::none()
            }
        }
    }

    /// Turn the polygon being marked out down.
    fn drop_draft(&mut self, layout: &Layout, damage: &mut Region) {
        if let Some(draft) = self.draft.take() {
            self.damage_picture(path_bounds(&draft.corners, Some(draft.to)), layout, damage);
        }
    }

    /// Lift the selection into a floating selection, leaving what an eraser
    /// would, as much as it chose, once it is put down. Nothing is written
    /// or copied.
    fn lift(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        let Some(chosen) = self.selection.clone() else {
            return false;
        };
        let left = self.eraser_ink(self.kind());
        let Some(picture) = self.document.picture() else {
            return false;
        };
        let Ok(floating) = Floating::lift(picture.canvas(), &chosen, left) else {
            self.state(
                "There is not enough memory to lift the selection",
                layout,
                damage,
            );
            return false;
        };
        self.held = Some(floating);
        self.selection = None;
        self.damage_picture(Some(chosen.bounds()), layout, damage);
        true
    }

    /// Put the floating selection down on a worker, then carry out `then` once it
    /// has landed; with nothing floating, carry it out now. The picture takes
    /// no edits until it is down.
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
            .map(|(picture, held)| (picture.canvas().try_clone(), held.try_clone()));
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
            Lands::Tiles(Settles::PutDown(then)),
            "put the selection down",
            layout,
            damage,
        )
    }

    /// Carry out what follows a floating selection's putting down.
    fn carry_on(&mut self, then: Then, layout: &Layout, damage: &mut Region) -> Outcome {
        match then {
            Then::Rest => Outcome::none(),
            Then::Act(action) => self.act(action, layout, damage),
            Then::Float(floating) => self.float(floating, layout, damage),
            Then::SaveThenClose => Outcome::asking(Request::SaveThenClose),
            Then::Enter(id, text) => self.carry_out_entry(id, &text, layout, damage),
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

    /// Forget the selection held.
    fn drop_selection(&mut self, layout: &Layout, damage: &mut Region) {
        if let Some(selection) = self.selection.take() {
            self.damage_picture(Some(selection.bounds()), layout, damage);
        }
    }

    /// Turn a floating selection down: one lifted goes back where it was, one
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

    /// End any drag as though the pointer had let go, but a marquee or a
    /// lasso, which only its own button's release marks: anything else that
    /// ends one turns it down.
    pub(crate) fn end_gesture(&mut self, layout: &Layout, damage: &mut Region) {
        if matches!(
            self.gesture,
            Some(Gesture::Marquee { .. } | Gesture::Lasso { .. } | Gesture::Gradient { .. })
        ) {
            self.cancel_gesture(layout, damage);
            return;
        }
        self.dragging = None;
        self.airbrush_due = None;
        match self.gesture.take() {
            Some(Gesture::Stroke { stroke, .. }) => {
                self.document.record_tiles(stroke.finish());
            }
            Some(Gesture::Shape {
                from,
                to,
                secondary,
            }) => self.put_shape(from, to, secondary, layout, damage),
            Some(Gesture::ZoomBox { from, to, .. }) => {
                damage.add(super::tools::screen_box(from, to).intersection(&layout.canvas()));
            }
            Some(
                Gesture::Marquee { .. }
                | Gesture::Lasso { .. }
                | Gesture::Gradient { .. }
                | Gesture::Move { .. }
                | Gesture::Pan { .. }
                | Gesture::CropNew { .. }
                | Gesture::CropAdjust { .. },
            )
            | None => {}
        }
    }

    /// Turn the drag under way down, keeping nothing it did: a stroke is
    /// taken back off the picture rather than left there unrecorded, a
    /// marquee marks nothing, and a dragged layer goes back where it was.
    fn cancel_gesture(&mut self, layout: &Layout, damage: &mut Region) {
        let preview = self.preview_bounds();
        self.dragging = None;
        self.airbrush_due = None;
        match self.gesture.take() {
            Some(Gesture::Stroke { stroke, .. }) => {
                if let Some(canvas) = self.document.canvas_mut() {
                    let restored = stroke.revert(canvas);
                    self.damage_picture(restored, layout, damage);
                }
            }
            Some(Gesture::Marquee { from, to, .. }) => {
                let marked = self.marquee_shape(from, to).bounds();
                self.damage_picture(Some(marked), layout, damage);
            }
            Some(Gesture::Lasso { points, .. }) => {
                self.damage_picture(path_bounds(&points, None), layout, damage);
            }
            Some(Gesture::Move { from, last }) => {
                self.shift_floating((from.0 - last.0, from.1 - last.1), layout, damage);
            }
            Some(Gesture::Gradient { .. }) => damage.add(layout.canvas()),
            Some(Gesture::Pan { scroll, .. }) => {
                self.scroll_to(Some(scroll.0), Some(scroll.1), layout, damage);
            }
            Some(Gesture::ZoomBox { from, to, .. }) => {
                damage.add(super::tools::screen_box(from, to).intersection(&layout.canvas()));
            }
            Some(Gesture::CropNew { before, .. }) => {
                self.crop = before;
                damage.add(layout.canvas());
            }
            Some(Gesture::CropAdjust { start, .. }) => {
                self.crop = Some(start);
                damage.add(layout.canvas());
            }
            Some(Gesture::Shape { .. }) | None => {}
        }
        self.damage_picture(preview, layout, damage);
    }

    /// The area an edit of the selection acts on: the floating selection's, else
    /// the selection's.
    fn selected_area(&self) -> Option<Bounds> {
        self.floating()
            .map(Floating::bounds)
            .or_else(|| self.selection.as_ref().map(Mask::bounds))
    }

    /// What a copy of the selection takes, as it stands: `None` with nothing
    /// selected.
    fn clip(&self) -> Option<Result<Clip, OutOfMemory>> {
        if let Some(floating) = &self.held {
            return Some(floating.try_clone().map(Clip::Floating));
        }
        let chosen = self.selection.clone()?;
        let picture = self.document.picture()?;
        Some(
            picture
                .canvas()
                .try_clone()
                .map(|canvas| Clip::Area { canvas, chosen }),
        )
    }

    /// The clipboard answered a paste with `pasted`, a picture decoded from
    /// it to float over a picture of the kind it names, or the reason there
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
        self.commit_text(layout, damage);
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
            _ => {
                // A form turned down takes its preview with it.
                self.preview = None;
                Outcome::none()
            }
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
                Ok((picture, format)) => {
                    Outcome::asking(Request::Own(Own::NewWindow { picture, format }))
                }
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::NewPage => match form.new_page_answer() {
                Ok(page) => self.add_page(page, layout, damage),
                Err(reason) => refused(self, form, &reason),
            },
            Purpose::SaveAs { then_close } => {
                let (format, settings) = form.save_as_answer();
                self.document.set_settings(settings);
                self.save_as = Some(format);
                Outcome::asking(Request::SaveWhere { then_close })
            }
            Purpose::Filter => match form.filter_answer() {
                Some(filter) => self.apply_filter(filter, layout, damage),
                None => Outcome::none(),
            },
            Purpose::Layer => match form.layer_answer() {
                Ok(shown) => self.reshow_layer(shown, layout, damage),
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
        }
    }

    /// Take the question of how to save: find, on a worker, what each format
    /// the document can be written as would not keep, and then put up the
    /// Save As sheet of them — or say why none is, which asks nothing
    /// further.
    pub(crate) fn ask_save_as(
        &mut self,
        then_close: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let entries = self.document.entries();
        let origin = self.document.origin();
        let none = SaveFormat::ALL
            .into_iter()
            .all(|format| writable_as(Some(format), entries, origin).is_err());
        if none {
            let refusal = writable_as(None, entries, origin)
                .err()
                .map_or_else(String::new, |refusal| alloc::format!("{refusal}"));
            self.state(&refusal, layout, damage);
            return Outcome::none();
        }
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let Ok(snapshot) = self.document.snapshot() else {
            self.state("There is not enough memory to save", layout, damage);
            return Outcome::none();
        };
        let lands = Lands::Sheet { then_close };
        self.begin_work(Compute::Survey { snapshot }, lands, "save", layout, damage)
    }

    /// The survey of what each format would not keep is in: put up the Save
    /// As sheet of them, the document's own natural format chosen first.
    fn put_up_save_as(
        &mut self,
        formats: Vec<(SaveFormat, Vec<crate::save::Loss>)>,
        then_close: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let natural = natural(self.document.entries(), self.document.origin());
        let choices = SaveChoices { formats };
        let form = Form::save_as(choices, natural, self.document.settings(), then_close);
        self.ask(form, layout, damage);
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

    /// Give the palette picture showing `palette`, as one step, its sprite
    /// details restating it.
    pub(super) fn change_palette(
        &mut self,
        palette: Vec<[u8; 4]>,
        layout: &Layout,
        damage: &mut Region,
    ) {
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
    pub(super) fn ask(&mut self, form: Form, layout: &Layout, damage: &mut Region) {
        // A form answers the picture as it stood when it opened, so none opens
        // while a worker may yet change it.
        if !self.idle(layout, damage) {
            return;
        }
        self.end_gesture(layout, damage);
        self.modal = Some(Modal::Form(Box::new(form)));
        damage.add(layout.window());
    }

    /// Ask a worker for `transform` of the picture showing.
    pub(super) fn transform(
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
        let palette = matches!(
            transform,
            Transform::Convert {
                depth: Depth::Indexed(_),
                ..
            } | Transform::Mask { .. }
        );
        if palette && !picture.single() {
            self.state(
                "A palette picture holds one layer, shown wholly: flatten the picture first",
                layout,
                damage,
            );
            return Outcome::none();
        }
        // The canvas a resize adds is filled beneath every layer, and left
        // clear over it.
        let for_layer = |index: usize| match transform {
            Transform::Resize {
                width,
                height,
                anchor,
                ..
            } if index > 0 => Transform::Resize {
                width,
                height,
                anchor,
                fill: Sample::Rgba([0; 4]),
            },
            other => other,
        };
        let mut layers = Vec::new();
        let shared = layers.try_reserve_exact(picture.layers().len()).is_ok()
            && picture.layers().iter().enumerate().all(|(index, layer)| {
                layer.canvas.try_clone().is_ok_and(|canvas| {
                    layers.push((canvas, for_layer(index)));
                    true
                })
            });
        if !shared {
            self.state(
                &alloc::format!("There is not enough memory to {what}"),
                layout,
                damage,
            );
            return Outcome::none();
        }
        self.begin_work(
            Compute::Transform { layers },
            Lands::Transform(transform),
            what,
            layout,
            damage,
        )
    }

    /// Add the page `new` describes after the one showing.
    fn add_page(&mut self, new: NewPicture, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let Ok(canvas) = new.canvas() else {
            self.state("There is not enough memory for the page", layout, damage);
            return Outcome::none();
        };
        self.insert_entry(Entry::Picture(Picture::plain(canvas)), layout, damage)
    }

    /// Put `entry` after the one showing.
    fn insert_entry(&mut self, entry: Entry, layout: &Layout, damage: &mut Region) -> Outcome {
        let at = self.document.current() + 1;
        match self.document.insert(at, entry) {
            Ok(()) => self.after_picture_change(layout, damage),
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
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
        let mut picture = Picture::plain(canvas);
        picture.sprite = Some(sprite);
        self.insert_entry(Entry::Picture(picture), layout, damage)
    }

    /// Show entry `index`: a floating selection was put down before this was
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
                None => self.filter_moved(layout, damage),
            };
        }
        let claimed = match self.keyboard() {
            Keyboard::Dock => self.dock_key(stroke, layout, scale, theme, damage),
            Keyboard::Bar => self.bar_key(stroke, layout, scale, theme, damage),
            Keyboard::Palette => self.palette_key(stroke, layout, scale, theme, damage),
            Keyboard::Picture if stroke.key == Key::Named(NamedKey::Tab) => {
                let forward = !stroke.modifiers.shift;
                self.walk_keyboard(Keyboard::Picture, forward, layout, scale, theme, damage);
                Some(Outcome::none())
            }
            Keyboard::Picture => None,
        };
        if let Some(outcome) = claimed {
            return outcome;
        }
        if let Some(outcome) = self.text_key(stroke.key, theme, layout, damage) {
            return outcome;
        }
        let bare = !stroke.modifiers.ctrl && !stroke.modifiers.alt && !stroke.modifiers.meta;
        if stroke.key == Key::Char(' ') && bare {
            // Held, it drags the view whatever the tool.
            self.space = true;
            return Outcome::none();
        }
        match shortcut(stroke.key, stroke.modifiers) {
            Some(action) => {
                self.commit_dock(layout, scale, theme, damage);
                self.commit_bar(layout, damage);
                self.act(action, layout, damage)
            }
            None => self.plain_key(stroke.key, layout, damage),
        }
    }

    /// A key for the colour dock: the picker takes every key it has a use
    /// for while it has the keyboard; Tab past either end of it carries the
    /// keyboard on, and an Escape it has nothing to take back for gives it
    /// back to the picture. `None` where the key is the window's.
    fn dock_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let outcome = self.picker.on_key(
            stroke.key,
            stroke.modifiers,
            layout.picker(),
            (scale, theme),
            damage,
        );
        if outcome != PickerOutcome::Ignored {
            return Some(self.picked(outcome, layout, damage));
        }
        self.leave_on(Keyboard::Dock, stroke, layout, scale, theme, damage)
    }

    /// A key for the tool-controls bar: a setting takes what it has a use for
    /// — every key a number field can be typed with — and Tab walks the
    /// settings and off the bar. `None` where the key is the window's.
    fn bar_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let outcome = self.bar.on_key(
            (stroke.key, stroke.modifiers),
            layout.bar(),
            &mut self.options,
            (scale, theme),
            damage,
        );
        match self.after_bar(outcome, theme, layout, damage) {
            BarOutcome::Taken | BarOutcome::Changed => Some(Outcome::none()),
            BarOutcome::Left { forward } => {
                self.walk_keyboard(Keyboard::Bar, forward, layout, scale, theme, damage);
                Some(Outcome::none())
            }
            BarOutcome::Ignored => {
                self.leave_on(Keyboard::Bar, stroke, layout, scale, theme, damage)
            }
        }
    }

    /// A key for the palette strip: the arrows walk its wells, each the
    /// primary ink as the mark reaches it. `None` where the key is the
    /// window's.
    fn palette_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        if let Some(SwatchAction::Selected { mark, index }) =
            self.swatches.on_key(stroke.key, layout.swatches(), damage)
        {
            self.choose_well(mark, index, layout, damage);
            return Some(Outcome::none());
        }
        self.leave_on(Keyboard::Palette, stroke, layout, scale, theme, damage)
    }

    /// Tab carries the keyboard on from `part` and Escape gives it back to
    /// the picture; `None` for any other key, which is the window's.
    fn leave_on(
        &mut self,
        part: Keyboard,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        match stroke.key {
            Key::Named(NamedKey::Tab) => {
                let forward = !stroke.modifiers.shift;
                self.walk_keyboard(part, forward, layout, scale, theme, damage);
                Some(Outcome::none())
            }
            Key::Named(NamedKey::Escape) => {
                self.release_keyboard(layout, scale, theme, damage);
                Some(Outcome::none())
            }
            _ => None,
        }
    }

    /// A key no shortcut claims: turn a drag or a selection down, or nudge a
    /// floating selection.
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
        self.settle_tool(layout, damage);
        let id = id.get();
        // Renaming touches no pixel, so a floating selection rides it out.
        if self.held.is_none() || id == RENAME_ENTRY {
            return self.carry_out_entry(id, text, layout, damage);
        }
        let mut held = String::new();
        if held.try_reserve_exact(text.len()).is_err() {
            self.state("There is not enough memory to do that", layout, damage);
            return Outcome::none();
        }
        held.push_str(text);
        self.put_down_then(Then::Enter(id, held), layout, damage)
    }

    /// Carry out what entry field `id` was committed holding: a floating
    /// selection was put down before this was reached, but for a rename.
    fn carry_out_entry(
        &mut self,
        id: u16,
        text: &str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        match id {
            GO_TO_ENTRY => self.go_to(text, layout, damage),
            GO_TO_LAYER => self.go_to_layer(text, layout, damage),
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
        let missing = if self.document.is_pages() {
            "There is no page of that number"
        } else {
            "There is no sprite of that name or number"
        };
        self.state(missing, layout, damage);
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
            || SpriteInfo::for_canvas(name, picture.canvas()),
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

    /// Turn down what the tool is in the middle of — a polygon's corners, a
    /// crop box — and set down text being typed: what every action but the
    /// view's own, and every entry committed, does first.
    fn settle_tool(&mut self, layout: &Layout, damage: &mut Region) {
        if self.draft.is_some() {
            self.drop_draft(layout, damage);
        }
        if self.crop.take().is_some() {
            damage.add(layout.canvas());
        }
        self.commit_text(layout, damage);
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
        if self.draft.is_some() {
            match action {
                Action::PutDown => return self.close_draft(layout, damage),
                Action::Delete => {
                    self.unplace_corner(layout, damage);
                    return Outcome::none();
                }
                _ => {}
            }
        }
        if self.crop.is_some() && action == Action::PutDown {
            return self.apply_crop(layout, damage);
        }
        if !action.spares(self.tool) {
            self.settle_tool(layout, damage);
        }
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
                self.adopt_selection(Mask::rect(Bounds::picture(width, height)), layout, damage);
            }
            Action::Deselect => self.drop_selection(layout, damage),
            Action::Delete => return self.delete_selection(layout, damage),
            Action::FillSelection => return self.fill_selection(layout, damage),
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
            Action::Invert => return self.adjust(crate::filter::Filter::Invert, layout, damage),
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
            Action::EditPrimary | Action::EditSecondary => {
                self.editing = if action == Action::EditPrimary {
                    SwatchMark::Primary
                } else {
                    SwatchMark::Secondary
                };
                self.sync_picker();
                self.picker.set_focused(true);
                damage.add(layout.dock());
            }
            Action::SwapColours => {
                core::mem::swap(&mut self.primary, &mut self.secondary);
                self.inks_changed(layout, damage);
            }
            Action::PreviousEntry => {
                let current = self.document.current();
                return self.show(current.saturating_sub(1), layout, damage);
            }
            Action::NextEntry => {
                let current = self.document.current();
                return self.show(current + 1, layout, damage);
            }
            Action::NewEntry if self.document.is_pages() => {
                self.ask(Form::new_page(self.picture_size()), layout, damage);
            }
            Action::NewEntry => {
                let name = SpriteName::new(SPRITE_STEM)
                    .and_then(|stem| free_name(&stem, self.document.names()))
                    .map_or_else(String::new, |name| name.to_string());
                self.ask(Form::new_sprite(&name, SPRITE_SIZE), layout, damage);
            }
            Action::DuplicateEntry => return self.duplicate(layout, damage),
            Action::DeleteEntry => return self.delete_sprite(layout, damage),
            Action::EntryUp | Action::EntryDown => {
                let current = self.document.current();
                let to = if action == Action::EntryUp {
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
                mark_grid(&mut self.commands, self.grid);
                damage.add(area);
                damage.add(layout.view_strip());
            }
            Action::NewLayer => return self.new_layer(layout, damage),
            Action::DuplicateLayer => return self.duplicate_layer(layout, damage),
            Action::DeleteLayer => return self.delete_layer(layout, damage),
            Action::LayerAbove | Action::LayerBelow => {
                return self.step_layer(action == Action::LayerAbove, layout, damage)
            }
            Action::RaiseLayer | Action::LowerLayer => {
                return self.move_layer(action == Action::RaiseLayer, layout, damage)
            }
            Action::MergeDown => return self.merge_down(layout, damage),
            Action::Flatten => return self.flatten(layout, damage),
            Action::ShowLayer => return self.toggle_layer(layout, damage),
            Action::LayerProperties => return self.ask_layer(layout, damage),
            // Put down before this was reached, which is all the action asks.
            Action::PutDown | Action::GoTo | Action::GoToLayer | Action::Rename => {}
            Action::Tool(tool) => return self.choose_tool(tool, layout, damage),
            Action::Adjust(index) => {
                if let Some(&filter) = crate::filter::Filter::ALL.get(index) {
                    return self.adjust(filter, layout, damage);
                }
            }
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
            Ok(Applied {
                damage: Damage::Layers,
                ..
            }) => return Self::layers_changed(layout, damage),
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
        let cleared = self.begin_work(
            work,
            Lands::Tiles(Settles::Cleared),
            CLEARING,
            layout,
            damage,
        );
        let Some(Request::Own(Own::Compute { job, work })) = cleared.request else {
            return Outcome::asking(Request::Own(Own::Copy(clip)));
        };
        Outcome::asking(Request::Own(Own::Cut { clip, job, work }))
    }

    /// Clear what is selected, as an eraser would; a floating selection is
    /// thrown away, and where it was lifted from keeps what the lift leaves.
    fn delete_selection(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        match self.clearing(layout, damage) {
            Some(work) => self.begin_work(
                work,
                Lands::Tiles(Settles::Cleared),
                CLEARING,
                layout,
                damage,
            ),
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
        let (chosen, ink) = if let Some(held) = &self.held {
            let Some(lifted) = held.lifted() else {
                let bounds = held.bounds();
                self.held = None;
                self.damage_picture(Some(bounds), layout, damage);
                return None;
            };
            lifted
        } else {
            (self.selection.clone()?, self.eraser_ink(self.kind()))
        };
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state("There is not enough memory to clear that", layout, damage);
            return None;
        };
        Some(Compute::Clear {
            canvas,
            chosen,
            ink,
        })
    }

    fn crop(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let (width, height) = self.picture_size();
        let Some(area) = self.selection.as_ref().map(|chosen| {
            chosen
                .bounds()
                .intersection(&Bounds::picture(width, height))
        }) else {
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
        self.insert_entry(Entry::Picture(copy), layout, damage)
    }

    fn delete_sprite(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.idle(layout, damage) {
            return Outcome::none();
        }
        let current = self.document.current();
        match self.document.remove(current) {
            Ok(()) => {
                // A selection floating over the sprite goes with it, and only then.
                self.held = None;
                self.after_picture_change(layout, damage)
            }
            Err(refusal) => {
                self.state(&alloc::format!("{refusal}"), layout, damage);
                Outcome::none()
            }
        }
    }

    /// Escape: turn a drag under way down, else a floating selection, else
    /// forget the selection.
    fn escape(&mut self, layout: &Layout, damage: &mut Region) {
        if self.gesture.is_some() {
            self.cancel_gesture(layout, damage);
            return;
        }
        if self.draft.is_some() {
            self.drop_draft(layout, damage);
            return;
        }
        if self.crop.take().is_some() {
            damage.add(layout.canvas());
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
        if let Some(layers) = menu.submenu("Layers", Plate::Root) {
            self.layer_rows(menu, layers);
        }
        if let Some(colours) = menu.submenu("Colours", Plate::Root) {
            menu.item(
                Action::EditPrimary,
                "Edit primary colour",
                "",
                picture,
                colours,
            );
            menu.item(
                Action::EditSecondary,
                "Edit secondary colour",
                "",
                picture,
                colours,
            );
            menu.item(Action::SwapColours, "Swap colours", "X", true, colours);
        }
        if let Some(adjust) = menu.submenu("Adjust", Plate::Root) {
            let palette = self.kind().palette().is_some();
            for (index, filter) in crate::filter::Filter::ALL.iter().enumerate() {
                let label = if filter.parameters().is_empty() {
                    String::from(filter.label())
                } else {
                    alloc::format!("{}\u{2026}", filter.label())
                };
                let can = picture && !(palette && filter.neighbourly());
                menu.item(Action::Adjust(index), &label, "", can, adjust);
            }
        }
        let pages = self.document.is_pages();
        if let Some(entries) = menu.submenu(if pages { "Pages" } else { "Sprites" }, Plate::Root) {
            self.entry_rows(menu, entries, pages);
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
            Action::FillSelection,
            "Fill with primary colour",
            "Alt+Backspace",
            self.document.picture().is_some(),
            plate,
        );
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

    /// The layers' rows: a palette picture holds one layer, shown wholly,
    /// so it offers none but the layer's own name.
    fn layer_rows(&self, menu: &mut MenuBuilder, plate: Plate) {
        let picture = self.document.picture();
        let colour = picture.is_some_and(|picture| picture.kind().palette().is_none());
        let (count, active) =
            picture.map_or((0, 0), |picture| (picture.layers().len(), picture.active()));
        let shown = picture
            .and_then(|picture| picture.layers().get(active))
            .is_some_and(|layer| layer.visible);
        let below = colour && active > 0;
        let above = colour && active + 1 < count;
        menu.item(Action::NewLayer, "New layer", "Ctrl+Shift+N", colour, plate);
        menu.item(Action::DuplicateLayer, "Duplicate layer", "", colour, plate);
        menu.item(Action::DeleteLayer, "Delete layer", "", count > 1, plate);
        menu.separator(plate);
        menu.item(
            Action::LayerAbove,
            "Layer above",
            "Ctrl+Page Up",
            above,
            plate,
        );
        menu.item(
            Action::LayerBelow,
            "Layer below",
            "Ctrl+Page Down",
            below,
            plate,
        );
        menu.entry(
            Action::GoToLayer,
            GO_TO_LAYER,
            "Go to layer\u{2026}",
            "",
            plate,
        );
        menu.separator(plate);
        menu.item(
            Action::RaiseLayer,
            "Raise layer",
            "Ctrl+Shift+Page Up",
            above,
            plate,
        );
        menu.item(
            Action::LowerLayer,
            "Lower layer",
            "Ctrl+Shift+Page Down",
            below,
            plate,
        );
        let mergeable =
            below && picture.is_some_and(|picture| picture.shows(active - 1..active + 1));
        menu.item(Action::MergeDown, "Merge down", "Ctrl+E", mergeable, plate);
        let flat = picture.is_none_or(Picture::single);
        menu.item(Action::Flatten, "Flatten", "Ctrl+Shift+E", !flat, plate);
        menu.separator(plate);
        if colour {
            menu.mark(Action::ShowLayer, "Show layer", "", shown, plate);
        } else {
            menu.item(Action::ShowLayer, "Show layer", "", false, plate);
        }
        menu.item(
            Action::LayerProperties,
            "Layer properties\u{2026}",
            "",
            colour,
            plate,
        );
    }

    /// The sprites' or the pages' rows: a page has no name, so it is gone
    /// to by number alone and never renamed.
    fn entry_rows(&self, menu: &mut MenuBuilder, plate: Plate, pages: bool) {
        let count = self.document.entries().len();
        let current = self.document.current();
        let many = count > 1;
        menu.item(
            Action::PreviousEntry,
            "Previous",
            "Page Up",
            current > 0,
            plate,
        );
        menu.item(
            Action::NextEntry,
            "Next",
            "Page Down",
            current + 1 < count,
            plate,
        );
        menu.entry(Action::GoTo, GO_TO_ENTRY, "Go to\u{2026}", "", plate);
        menu.separator(plate);
        let new = if pages {
            "New page\u{2026}"
        } else {
            "New sprite\u{2026}"
        };
        menu.item(Action::NewEntry, new, "", true, plate);
        menu.item(
            Action::DuplicateEntry,
            "Duplicate",
            "",
            self.document.picture().is_some(),
            plate,
        );
        if !pages {
            let name = self
                .document
                .entry()
                .name()
                .map_or_else(String::new, ToString::to_string);
            menu.entry(Action::Rename, RENAME_ENTRY, "Rename\u{2026}", &name, plate);
        }
        menu.item(Action::DeleteEntry, "Delete", "", many, plate);
        menu.item(Action::EntryUp, "Move up", "", current > 0, plate);
        menu.item(
            Action::EntryDown,
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
    menu.separator(plate);
    menu.item(Action::Close, "Close", "Ctrl+W", true, plate);
}

/// A part of the window the keyboard can be in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Keyboard {
    /// The picture: tools' and commands' keys, nudges.
    Picture,
    /// The tool-controls bar's settings.
    Bar,
    /// The palette strip's wells.
    Palette,
    /// The colour dock's picker.
    Dock,
}

impl Keyboard {
    /// The order Tab walks.
    const ORDER: [Self; 4] = [Self::Picture, Self::Bar, Self::Palette, Self::Dock];
}

/// The tool a strip's primary activation chose.
fn activated(outcome: ToolbarOutcome) -> Option<usize> {
    match outcome {
        ToolbarOutcome::Activated(action) if action.part == ToolActivation::Primary => {
            Some(action.index)
        }
        _ => None,
    }
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
/// The pixels a straight edge from `a` to `b` may cross.
pub(super) fn segment(a: Fx, b: Fx) -> Bounds {
    let (ax, ay) = a.pixel();
    let (bx, by) = b.pixel();
    Bounds {
        x0: ax.min(bx),
        y0: ay.min(by),
        x1: ax.max(bx) + 1,
        y1: ay.max(by) + 1,
    }
}

/// The pixels a path through `points`, and on to `to`, may cross.
fn path_bounds(points: &[Fx], to: Option<Fx>) -> Option<Bounds> {
    points
        .iter()
        .chain(to.as_ref())
        .map(|&point| segment(point, point))
        .reduce(|a, b| a.union(&b))
}

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
        Key::Named(NamedKey::Backspace) if modifiers.alt => Some(Action::FillSelection),
        Key::Named(NamedKey::Delete | NamedKey::Backspace) => Some(Action::Delete),
        Key::Named(named @ (NamedKey::PageUp | NamedKey::PageDown)) if ctrl => {
            let up = named == NamedKey::PageUp;
            Some(match (up, modifiers.shift) {
                (true, true) => Action::RaiseLayer,
                (false, true) => Action::LowerLayer,
                (true, false) => Action::LayerAbove,
                (false, false) => Action::LayerBelow,
            })
        }
        Key::Named(NamedKey::PageUp) => Some(Action::PreviousEntry),
        Key::Named(NamedKey::PageDown) => Some(Action::NextEntry),
        Key::Char(ch) if ctrl => match (ch.to_ascii_lowercase(), modifiers.shift) {
            ('n', true) => Some(Action::NewLayer),
            ('n', false) => Some(Action::NewPicture),
            ('e', true) => Some(Action::Flatten),
            ('e', false) => Some(Action::MergeDown),
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
