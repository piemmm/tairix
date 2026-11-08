//! The Colour pane beyond its wells and picker: swapping and resetting the
//! inks, a one-shot pick from the picture, the picker's view and the model its
//! fields show, and the colours last settled. The pane's parts are laid out
//! whole in its own layout, which its dock scrolls through its body: what
//! reaches them is mapped into that layout, and what they report is mapped
//! back to where the body shows it.

use alloc::vec;
use alloc::vec::Vec;

use tairix_colour::Rgba;
use tairix_controls::{
    damage, Button, ColourModel, Keystroke, PickerView, SwatchAction, SwatchGrid, SwatchMark,
};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_raster::Color;
use tairix_theme::Theme;

use super::{Outcome, View};
use crate::colour::{nearest, Ink};
use crate::layout::Faces;
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
    /// Report `rect`, in the Colour pane's own layout, where the pane shows
    /// it.
    pub(super) fn damage_colour(&self, rect: Rect, damage: &mut Region) {
        if let Some(shown) = self.held_view(PaneKind::Colour, false).to_window(rect) {
            damage.add(shown);
        }
    }

    /// The pointer on the colour pane's buttons and choices: `None` where the
    /// event is none of theirs.
    pub(super) fn colour_controls_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let bounds = self.layout.colour_controls();
        let held = self.colour_controls.holding() || self.colour_controls.listing();
        if bounds.is_empty() && !held {
            return None;
        }
        let view = self.held_view(PaneKind::Colour, held);
        let room = self.list_room(PaneKind::Colour);
        let context = (Faces::of(theme, scale), scale, theme);
        let mut drew = damage::sink();
        let outcome = self.colour_controls.on_pointer(
            &view.event_in_layout(event),
            (bounds, room),
            context,
            &mut drew,
        );
        view.report(&drew, damage);
        match outcome {
            PanelOutcome::Ignored => {
                let pressed = matches!(
                    event,
                    InputEvent::PointerPressed {
                        button: PointerButton::Primary
                    }
                );
                let over = view
                    .to_content(self.pointer)
                    .is_some_and(|at| bounds.contains(at));
                (pressed && over).then(Outcome::none)
            }
            outcome => Some(self.colour_controls_answered(outcome, damage)),
        }
    }

    /// A key while the colour pane's buttons and choices have the keyboard.
    pub(super) fn colour_controls_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let context = (Faces::of(theme, scale), scale, theme);
        let listing = self.colour_controls.listing();
        let view = self.held_view(PaneKind::Colour, listing);
        let room = self.list_room(PaneKind::Colour);
        let mut drew = damage::sink();
        let outcome = self.colour_controls.on_key(
            stroke,
            (self.layout.colour_controls(), room),
            context,
            &mut drew,
        );
        view.report(&drew, damage);
        match outcome {
            PanelOutcome::Ignored => None,
            PanelOutcome::Left { forward } => {
                self.walk_keyboard(
                    super::input::Keyboard::Colours,
                    forward,
                    scale,
                    theme,
                    damage,
                );
                Some(Outcome::none())
            }
            outcome => Some(self.colour_controls_answered(outcome, damage)),
        }
    }

    /// Take the keyboard from the colour pane's buttons and choices.
    pub(super) fn release_colour_controls(
        &mut self,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let context = (Faces::of(theme, scale), scale, theme);
        let view = self.held_view(PaneKind::Colour, self.colour_controls.listing());
        let mut drew = damage::sink();
        let _ = self
            .colour_controls
            .blur(self.layout.colour_controls(), context, &mut drew);
        view.report(&drew, damage);
    }

    /// Give the colour pane's buttons and choices the keyboard, scrolled into
    /// view.
    pub(super) fn enter_colour_controls(&mut self, forward: bool, damage: &mut Region) -> bool {
        let bounds = self.layout.colour_controls();
        if bounds.is_empty() {
            return false;
        }
        self.reveal_body(PaneKind::Colour, bounds, damage);
        let mut drew = damage::sink();
        let entered = self.colour_controls.enter_focus(forward, bounds, &mut drew);
        self.held_view(PaneKind::Colour, false)
            .report(&drew, damage);
        entered
    }

    fn colour_controls_answered(&mut self, outcome: PanelOutcome, damage: &mut Region) -> Outcome {
        let PanelOutcome::Event(event) = outcome else {
            return Outcome::none();
        };
        match event {
            PanelEvent::Pressed {
                part: BUTTONS,
                button: SWAP,
            } => self.act(super::Action::SwapColours, damage),
            PanelEvent::Pressed {
                part: BUTTONS,
                button: RESET,
            } => self.act(super::Action::ResetColours, damage),
            PanelEvent::Pressed {
                part: BUTTONS,
                button: PICK,
            } => self.act(super::Action::PickColour, damage),
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
    pub(super) fn reset_colours(&mut self, damage: &mut Region) {
        let (black, white) = ([0, 0, 0, u8::MAX], [u8::MAX; 4]);
        (self.primary, self.secondary) = match self.kind().palette() {
            Some(palette) => (
                Ink::Index(nearest(palette, black)),
                Ink::Index(nearest(palette, white)),
            ),
            None => (Ink::Colour(black), Ink::Colour(white)),
        };
        self.inks_changed(SwatchMark::Primary, damage);
    }

    /// Take up the one-shot pick: the next press on the picture takes the
    /// colour the layers show there into the ink the pane edits, and the
    /// tool in use carries on; asked again, put it down.
    pub(super) fn toggle_colour_pick(&mut self, damage: &mut Region) {
        self.picking_colour = !self.picking_colour;
        self.damage_colour(self.layout.colour_controls(), damage);
    }

    /// The one-shot pick's press at `at`.
    pub(super) fn colour_picked_at(&mut self, at: Fx, damage: &mut Region) {
        self.picking_colour = false;
        self.damage_colour(self.layout.colour_controls(), damage);
        self.pick(at, self.editing == SwatchMark::Secondary, damage);
    }

    /// Remember `colour` as the most recent, once.
    pub(super) fn remember_colour(&mut self, colour: Rgba, damage: &mut Region) {
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
        self.damage_colour(self.layout.recents(), damage);
    }

    /// Where the recent colours are drawn, in the pane's own layout: the rows
    /// they fill, from the top of the room the pane keeps for them all.
    pub(crate) fn recents_rect(&self) -> Rect {
        let room = self.layout.recents();
        let height = self
            .recent_grid
            .height_for_width(room.width)
            .min(room.height);
        Rect::new(room.left(), room.top(), room.width, height)
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
        damage: &mut Region,
    ) -> Option<Outcome> {
        let bounds = self.recents_rect();
        let view = self.held_view(PaneKind::Colour, false);
        let mut drew = damage::sink();
        let chosen = self.recent_grid.on_pointer(
            &view.event_in_layout(event),
            bounds,
            SwatchMark::Primary,
            &mut drew,
        );
        view.report(&drew, damage);
        if let Some(SwatchAction::Selected { index, .. }) = chosen {
            self.take_recent(index, damage);
            return Some(Outcome::none());
        }
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        let over = view
            .to_content(self.pointer)
            .is_some_and(|at| bounds.contains(at));
        (pressed && over).then(Outcome::none)
    }

    /// A key while the recent colours have the keyboard.
    pub(super) fn recents_key(
        &mut self,
        stroke: Keystroke,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let mut drew = damage::sink();
        let chosen = self
            .recent_grid
            .on_key(stroke.key, self.recents_rect(), &mut drew);
        self.held_view(PaneKind::Colour, false)
            .report(&drew, damage);
        let SwatchAction::Selected { index, .. } = chosen?;
        self.take_recent(index, damage);
        Some(Outcome::none())
    }

    fn take_recent(&mut self, index: usize, damage: &mut Region) {
        let Some(&colour) = self.recents.get(index) else {
            return;
        };
        let ink = Ink::of_colour(colour.to_array());
        match self.editing {
            SwatchMark::Primary => self.primary = ink,
            SwatchMark::Secondary => self.secondary = ink,
        }
        self.inks_changed(self.editing, damage);
    }

    /// Give the recent colours the keyboard, where there are any to take it.
    pub(super) fn enter_recents(&mut self, damage: &mut Region) -> bool {
        let bounds = self.recents_rect();
        if self.recent_grid.is_empty()
            || bounds.is_empty()
            || !self.recent_grid.state().is_actionable()
        {
            return false;
        }
        self.reveal_body(PaneKind::Colour, bounds, damage);
        self.recent_grid.set_focused(true);
        self.damage_colour(bounds, damage);
        true
    }

    /// Take the keyboard from the recent colours.
    pub(super) fn release_recents(&mut self, damage: &mut Region) {
        if self.recent_grid.state().focus.focused {
            self.recent_grid.set_focused(false);
            self.damage_colour(self.recents_rect(), damage);
        }
    }

    /// A key while the recent colours have the keyboard: Tab walks on, the
    /// arrows choose a well.
    pub(super) fn recents_walk_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        if stroke.key == tairix_input::Key::Named(tairix_input::NamedKey::Tab) {
            let forward = !stroke.modifiers.shift;
            self.walk_keyboard(
                super::input::Keyboard::Recents,
                forward,
                scale,
                theme,
                damage,
            );
            return Some(Outcome::none());
        }
        self.recents_key(stroke, damage)
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
