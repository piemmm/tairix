//! The Colour pane beyond its wells and picker: swapping and resetting the
//! inks, a one-shot pick from the picture, the picker's view and the model its
//! fields show, and the colours last settled.

use alloc::vec;
use alloc::vec::Vec;

use tairix_colour::Rgba;
use tairix_controls::{
    Button, ColourModel, Keystroke, PickerView, SwatchAction, SwatchGrid, SwatchMark,
};
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::Color;
use tairix_theme::Theme;

use super::{Outcome, View};
use crate::colour::{nearest, Ink};
use crate::layout::{Faces, Layout};
use crate::pane::PaneKind;
use crate::panel::{Panel, PanelEvent, PanelOutcome, Part};
use crate::shape::Point as Fx;

/// The most colours the pane remembers.
pub(super) const RECENTS: usize = 16;

/// The recent colours to a row.
pub(super) const RECENT_COLUMNS: usize = 8;

/// The colour pane's panel: its buttons, then its two choices.
const BUTTONS: usize = 0;
const VIEW: usize = 1;
const MODEL: usize = 2;

/// The buttons, in their row's order.
const SWAP: usize = 0;
const RESET: usize = 1;
const PICK: usize = 2;

/// The colour pane's panel, choosing `view` and `model`.
pub(super) fn colour_panel(view: PickerView, model: ColourModel) -> Panel {
    let views: Vec<&str> = PickerView::ALL.iter().map(|view| view.label()).collect();
    let models: Vec<&str> = ColourModel::ALL.iter().map(|model| model.label()).collect();
    let index_of = |found: Option<usize>| found.unwrap_or(0);
    Panel::new(vec![
        Part::Buttons(vec![
            Button::labelled("Swap"),
            Button::labelled("Reset"),
            Button::labelled("Pick"),
        ]),
        Part::choice(
            "View",
            &views,
            index_of(PickerView::ALL.iter().position(|&at| at == view)),
        ),
        Part::choice(
            "Fields",
            &models,
            index_of(ColourModel::ALL.iter().position(|&at| at == model)),
        ),
    ])
}

/// An empty well grid for the recent colours.
pub(super) fn recent_grid() -> SwatchGrid {
    SwatchGrid::new(RECENT_COLUMNS, Vec::new())
}

impl View {
    /// The pointer on the colour pane's buttons and choices: `None` where the
    /// event is none of theirs.
    pub(super) fn colour_controls_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let bounds = layout.colour_controls();
        if bounds.is_empty() && !self.colour_controls.holding() && !self.colour_controls.listing() {
            return None;
        }
        let context = (Faces::of(theme, scale), scale, theme);
        let outcome = self.colour_controls.on_pointer(
            event,
            (bounds, layout.pane_window(PaneKind::Colour)),
            context,
            damage,
        );
        match outcome {
            PanelOutcome::Ignored => {
                let pressed = matches!(
                    event,
                    InputEvent::PointerPressed {
                        button: PointerButton::Primary
                    }
                );
                (pressed && bounds.contains(self.pointer)).then(Outcome::none)
            }
            outcome => Some(self.colour_controls_answered(outcome, layout, damage)),
        }
    }

    /// A key while the colour pane's buttons and choices have the keyboard.
    pub(super) fn colour_controls_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let context = (Faces::of(theme, scale), scale, theme);
        let outcome = self.colour_controls.on_key(
            stroke,
            (
                layout.colour_controls(),
                layout.pane_window(PaneKind::Colour),
            ),
            context,
            damage,
        );
        match outcome {
            PanelOutcome::Ignored => None,
            PanelOutcome::Left { forward } => {
                self.walk_keyboard(
                    super::input::Keyboard::Colours,
                    forward,
                    layout,
                    scale,
                    theme,
                    damage,
                );
                Some(Outcome::none())
            }
            outcome => Some(self.colour_controls_answered(outcome, layout, damage)),
        }
    }

    /// Take the keyboard from the colour pane's buttons and choices.
    pub(super) fn release_colour_controls(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let context = (Faces::of(theme, scale), scale, theme);
        let _ = self
            .colour_controls
            .blur(layout.colour_controls(), context, damage);
    }

    /// Give the colour pane's buttons and choices the keyboard.
    pub(super) fn enter_colour_controls(
        &mut self,
        forward: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> bool {
        let bounds = layout.colour_controls();
        !bounds.is_empty() && self.colour_controls.enter_focus(forward, bounds, damage)
    }

    fn colour_controls_answered(
        &mut self,
        outcome: PanelOutcome,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let PanelOutcome::Event(event) = outcome else {
            return Outcome::none();
        };
        match event {
            PanelEvent::Pressed {
                part: BUTTONS,
                button: SWAP,
            } => self.act(super::Action::SwapColours, layout, damage),
            PanelEvent::Pressed {
                part: BUTTONS,
                button: RESET,
            } => self.act(super::Action::ResetColours, layout, damage),
            PanelEvent::Pressed {
                part: BUTTONS,
                button: PICK,
            } => self.act(super::Action::PickColour, layout, damage),
            PanelEvent::Chosen { part: VIEW, index } => {
                let view = PickerView::ALL.get(index).copied().unwrap_or_default();
                self.picker.set_view(view);
                Outcome::relaid()
            }
            PanelEvent::Chosen { part: MODEL, index } => {
                let model = ColourModel::ALL.get(index).copied().unwrap_or_default();
                self.picker.set_model(model);
                Outcome::relaid()
            }
            _ => Outcome::none(),
        }
    }

    /// Put the inks back to black and white: on a palette picture, the
    /// entries nearest them.
    pub(super) fn reset_colours(&mut self, layout: &Layout, damage: &mut Region) {
        let (black, white) = ([0, 0, 0, u8::MAX], [u8::MAX; 4]);
        (self.primary, self.secondary) = match self.kind().palette() {
            Some(palette) => (
                Ink::Index(nearest(palette, black)),
                Ink::Index(nearest(palette, white)),
            ),
            None => (Ink::Colour(black), Ink::Colour(white)),
        };
        self.inks_changed(layout, damage);
    }

    /// Take up the one-shot pick: the next press on the picture takes the
    /// colour the layers show there into the ink the pane edits, and the
    /// tool in use carries on; asked again, put it down.
    pub(super) fn toggle_colour_pick(&mut self, layout: &Layout, damage: &mut Region) {
        self.picking_colour = !self.picking_colour;
        damage.add(layout.colour_controls());
    }

    /// The one-shot pick's press at `at`.
    pub(super) fn colour_picked_at(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        self.picking_colour = false;
        damage.add(layout.colour_controls());
        self.pick(at, self.editing == SwatchMark::Secondary, layout, damage);
    }

    /// Remember `colour` as the most recent, once.
    pub(super) fn remember_colour(&mut self, colour: Rgba, layout: &Layout, damage: &mut Region) {
        if let Some(at) = self.recents.iter().position(|&held| held == colour) {
            self.recents.remove(at);
        } else if self.recents.len() == RECENTS {
            self.recents.pop();
        } else if self.recents.try_reserve(1).is_err() {
            return;
        }
        self.recents.insert(0, colour);
        let mut wells = Vec::new();
        if wells.try_reserve_exact(self.recents.len()).is_err() {
            return;
        }
        wells.extend(self.recents.iter().map(|&held| Color::from(held)));
        self.recent_grid.adopt_colours(RECENT_COLUMNS, wells);
        self.recent_grid.adopt_selected(None);
        damage.add(layout.recents());
    }

    /// Where the recent colours are drawn: the rows they fill, from the top
    /// of the room the pane keeps for them all.
    pub(crate) fn recents_rect(&self, layout: &Layout) -> tairix_geometry::Rect {
        let room = layout.recents();
        let height = self
            .recent_grid
            .height_for_width(room.width)
            .min(room.height);
        tairix_geometry::Rect::new(room.left(), room.top(), room.width, height)
    }

    /// The recent colours, for the painter.
    pub(crate) const fn recent_colours(&self) -> &SwatchGrid {
        &self.recent_grid
    }

    /// The colour pane's buttons and choices, for the painter.
    pub(crate) const fn colour_controls(&self) -> &Panel {
        &self.colour_controls
    }

    /// The pointer on the recent colours: a well chosen becomes the ink the
    /// pane edits, on a colour picture.
    pub(super) fn recents_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let bounds = self.recents_rect(layout);
        let chosen = self
            .recent_grid
            .on_pointer(event, bounds, SwatchMark::Primary, damage);
        if let Some(SwatchAction::Selected { index, .. }) = chosen {
            self.take_recent(index, layout, damage);
            return Some(Outcome::none());
        }
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        (pressed && bounds.contains(self.pointer)).then(Outcome::none)
    }

    /// A key while the recent colours have the keyboard.
    pub(super) fn recents_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let chosen = self
            .recent_grid
            .on_key(stroke.key, self.recents_rect(layout), damage)?;
        let SwatchAction::Selected { index, .. } = chosen;
        self.take_recent(index, layout, damage);
        Some(Outcome::none())
    }

    fn take_recent(&mut self, index: usize, layout: &Layout, damage: &mut Region) {
        let Some(&colour) = self.recents.get(index) else {
            return;
        };
        let ink = Ink::of_colour(colour.to_array());
        match self.editing {
            SwatchMark::Primary => self.primary = ink,
            SwatchMark::Secondary => self.secondary = ink,
        }
        self.inks_changed(layout, damage);
    }

    /// Give the recent colours the keyboard, where there are any to take it.
    pub(super) fn enter_recents(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        let bounds = self.recents_rect(layout);
        if self.recent_grid.is_empty()
            || bounds.is_empty()
            || !self.recent_grid.state().is_actionable()
        {
            return false;
        }
        self.recent_grid.set_focused(true);
        damage.add(bounds);
        true
    }

    /// Take the keyboard from the recent colours.
    pub(super) fn release_recents(&mut self, layout: &Layout, damage: &mut Region) {
        if self.recent_grid.state().focus.focused {
            self.recent_grid.set_focused(false);
            damage.add(self.recents_rect(layout));
        }
    }

    /// A key while the recent colours have the keyboard: Tab walks on, the
    /// arrows choose a well.
    pub(super) fn recents_walk_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        if stroke.key == tairix_input::Key::Named(tairix_input::NamedKey::Tab) {
            let forward = !stroke.modifiers.shift;
            self.walk_keyboard(
                super::input::Keyboard::Recents,
                forward,
                layout,
                scale,
                theme,
                damage,
            );
            return Some(Outcome::none());
        }
        self.recents_key(stroke, layout, damage)
    }

    /// Offer the recent colours on a colour picture alone: a palette
    /// picture's inks are its entries.
    pub(super) fn sync_recents(&mut self) {
        let mut state = self.recent_grid.state();
        state.enabled = self.kind().palette().is_none();
        if state != self.recent_grid.state() {
            if !state.enabled {
                state.focus.focused = false;
            }
            self.recent_grid.set_state(state);
        }
    }
}
