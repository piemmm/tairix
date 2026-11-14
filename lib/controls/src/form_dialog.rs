//! A dialog window whose client is a column of field groups above its
//! actions: the routing every form dialog shares.
//!
//! A press goes to the part it began in, the fields or the actions, and its
//! release follows it there, so a press that takes an open list away presses
//! nothing beneath it. An open list is modal and keeps every key until it
//! closes; otherwise Escape dismisses the dialog and an Enter no field takes
//! activates the trailing, recommended action.

use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::column::{ColumnAction, ColumnPlace, FieldColumn};
use crate::decision::{Dialog, DialogAction};
use crate::form::{FieldControl, FieldRow};
use crate::text::Keystroke;

/// What a [`FormDialog`] was asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormAction {
    /// A row asked for something; the owner takes it.
    Field(ColumnAction),
    /// The action at this index was activated.
    Action(usize),
    /// Escape, with no list open.
    Dismissed,
}

/// The part a press began in, which its release goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Fields,
    Actions,
}

/// A form dialog's state: the dialog's bands, the column in its content band,
/// and the press under way.
#[derive(Debug)]
pub struct FormDialog {
    dialog: Dialog,
    column: FieldColumn,
    /// The window's width, in logical pixels.
    width: u32,
    armed: Option<Part>,
    pointer: Point,
}

impl FormDialog {
    /// `dialog`, its content band holding `column`, `width` logical pixels
    /// wide.
    #[must_use]
    pub fn new(dialog: Dialog, column: FieldColumn, width: u32) -> Self {
        Self {
            dialog,
            column,
            width,
            armed: None,
            pointer: Point::ORIGIN,
        }
    }

    /// The dialog's bands.
    #[must_use]
    pub const fn dialog(&self) -> &Dialog {
        &self.dialog
    }

    /// The dialog's bands, to change.
    pub fn dialog_mut(&mut self) -> &mut Dialog {
        &mut self.dialog
    }

    /// The fields.
    #[must_use]
    pub const fn column(&self) -> &FieldColumn {
        &self.column
    }

    /// The fields, to change.
    pub fn column_mut(&mut self) -> &mut FieldColumn {
        &mut self.column
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(self.width);
        let band = self.dialog.content_width(width, scale, theme);
        let content = self.column.measured_height(band, scale, theme);
        (
            width,
            self.dialog.height_for_content(content, width, scale, theme),
        )
    }

    /// The client, and where the groups stack down it: a choice list may hang
    /// anywhere in the client, over the actions included, since it is modal.
    #[must_use]
    pub fn place<'t>(&self, scale: Scale, theme: &'t Theme) -> (Rect, ColumnPlace<'t>) {
        let (width, height) = self.extent(scale, theme);
        let bounds = Rect::new(0, 0, width, height);
        let content = self
            .dialog
            .content_rect(bounds, scale, theme)
            .unwrap_or(Rect::EMPTY);
        let place = ColumnPlace {
            bounds: content,
            viewport: bounds,
            scale,
            theme,
        };
        (bounds, place)
    }

    /// Paint the whole client.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        let (bounds, place) = self.place(scale, theme);
        self.dialog.render(surface, bounds, scale, theme);
        self.column.render(surface, place);
        self.column.render_popup(surface, place);
    }

    /// Take `event` at monotonic `at_ns`: what the dialog was asked, and what
    /// it redrew in `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<FormAction> {
        let (bounds, place) = self.place(scale, theme);
        if let Some(stroke) = Keystroke::pressed(*event, at_ns) {
            return self.on_key(stroke, place, damage);
        }
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let listing = self.column.is_listing();
        if matches!(event, InputEvent::PointerPressed { .. }) {
            self.armed = Some(if listing || place.bounds.contains(self.pointer) {
                Part::Fields
            } else {
                Part::Actions
            });
        }
        let mut acted = None;
        if self.armed != Some(Part::Actions) {
            acted = self
                .column
                .on_pointer(event, place, damage)
                .map(FormAction::Field);
        }
        if acted.is_none() && self.armed != Some(Part::Fields) && !listing {
            acted = self
                .dialog
                .on_pointer(event, bounds, scale, theme, damage)
                .map(|DialogAction::ActionActivated { index }| FormAction::Action(index));
        }
        if matches!(
            event,
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
        ) {
            self.armed = None;
        }
        acted
    }

    fn on_key(
        &mut self,
        stroke: Keystroke,
        place: ColumnPlace<'_>,
        damage: &mut Region,
    ) -> Option<FormAction> {
        let listing = self.column.is_listing();
        if stroke.key == Key::Named(NamedKey::Escape) && !listing {
            return Some(FormAction::Dismissed);
        }
        if let Some(action) = self.column.on_key(stroke, place, damage) {
            return Some(FormAction::Field(action));
        }
        if listing {
            return None;
        }
        if let Some(DialogAction::ActionActivated { index }) = self.dialog.on_key(stroke.key) {
            return Some(FormAction::Action(index));
        }
        let trailing = self.dialog.actions().len().checked_sub(1)?;
        (stroke.key == Key::Named(NamedKey::Enter)).then_some(FormAction::Action(trailing))
    }

    /// Show the switch at `group`'s `row` as `on`: a switch only asks, so the
    /// value it shows is the one its owner took.
    pub fn show_toggle(
        &mut self,
        (group, row): (usize, usize),
        on: bool,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        let (_, place) = self.place(scale, theme);
        let drawn = self
            .column
            .layouts(place)
            .into_iter()
            .find_map(|(at, layout)| (at == group).then_some(layout))
            .and_then(|layout| {
                self.column
                    .groups()
                    .get(group)?
                    .row_rect(row, layout, scale, theme)
            });
        let switch = self
            .column
            .groups_mut()
            .get_mut(group)
            .and_then(|group| group.rows_mut().get_mut(row))
            .map(FieldRow::control_mut);
        if let Some(FieldControl::Toggle(toggle)) = switch {
            if toggle.is_on() != on {
                toggle.set_on(on);
                if let Some(rect) = drawn {
                    damage.add(rect);
                }
            }
        }
    }

    /// When a row's control next changes on its own.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.column.deadline_ns()
    }

    /// Bring every row's control up to `now_ns`, reporting what it redrew.
    pub fn advance(&mut self, now_ns: u64, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let (_, place) = self.place(scale, theme);
        self.column.advance(now_ns, place, damage);
    }
}

#[cfg(test)]
#[path = "form_dialog_tests.rs"]
mod tests;
