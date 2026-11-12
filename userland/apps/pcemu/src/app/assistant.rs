//! The new-machine assistant: a dialog window's client offering the four
//! machines, the name the one chosen is given, and whether a hard disk is made
//! for it.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ControlRole, ControlState, Dialog, DialogAction, FieldAction,
    FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow, Keystroke, TextField,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::library::MAX_NAME;
use super::shelf::describe_config;
use crate::machine::Template;

/// The machines offered, in order, and what each is called.
pub const TEMPLATES: [(Template, &str); 4] = [
    (Template::IbmPc, "IBM Personal Computer"),
    (Template::IbmXt, "IBM Personal Computer XT"),
    (Template::TurboXt, "Turbo XT"),
    (Template::Workstation186, "80186 workstation"),
];

/// The machine offered first: the quickest the default disk boots on.
pub const OFFERED: usize = 2;

const DISKS: [&str; 2] = ["A new 30 MB hard disk", "None"];
/// What a machine with no fixed disk controller is offered.
const NO_DISK: [&str; 1] = ["None"];

/// The dialog's title, which the desktop's frame carries.
pub const TITLE: &str = "New Machine";

/// The window's width, in logical pixels.
const WIDTH: u32 = 560;

/// The rows, in order.
const NAME: usize = 0;
const MODEL: usize = 1;
const DISK: usize = 2;

/// What the assistant answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Nothing is made.
    Cancel,
    /// Make a `template` machine called `name`, a hard disk made for it where
    /// `disk`.
    Create {
        /// The machine.
        template: Template,
        /// What it is called.
        name: String,
        /// Whether it has the default disk.
        disk: bool,
    },
}

/// Which part a press began in, which its release goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Fields,
    Actions,
}

/// The assistant's state.
#[derive(Debug)]
pub struct Assistant {
    dialog: Dialog,
    fields: FieldGroup,
    /// The name each template is offered under.
    names: [String; 4],
    template: usize,
    disk: bool,
    armed: Option<Part>,
    pointer: Point,
}

/// Whether `template` (an index of [`TEMPLATES`]) has a controller a fixed
/// disk could be made for.
fn takes_disk(template: usize) -> bool {
    TEMPLATES[template].0.config().fixed.is_some()
}

fn about(template: usize, disk: bool) -> String {
    let config = TEMPLATES[template].0.config();
    describe_config(&config, usize::from(disk && takes_disk(template)))
}

/// The disk row a machine of `template` is offered.
fn disk_row(template: usize, disk: bool) -> FieldRow {
    if takes_disk(template) {
        FieldRow::choice("Hard disk", &DISKS, usize::from(!disk))
    } else {
        let mut row = FieldRow::choice("Hard disk", &NO_DISK, 0)
            .with_description("This machine has no hard disk controller.");
        row.set_state(ControlState::disabled());
        row
    }
}

impl Assistant {
    /// The assistant, offering the Turbo XT with a hard disk; `names` gives
    /// the name each of [`TEMPLATES`] is offered under.
    #[must_use]
    pub fn new(names: [String; 4]) -> Self {
        let models: Vec<&str> = TEMPLATES.iter().map(|(_, name)| *name).collect();
        let name = TextField::new()
            .with_text(&names[OFFERED])
            .with_max_len(MAX_NAME);
        let fields = FieldGroup::new(
            "MACHINE",
            vec![
                FieldRow::new("Name", FieldControl::Text(name)),
                FieldRow::choice("Model", &models, OFFERED).with_description(about(OFFERED, true)),
                disk_row(OFFERED, true),
            ],
        );
        let dialog = Dialog::new(TITLE).in_window().with_actions(vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Create")),
                ControlRole::Recommended,
            ),
        ]);
        Self {
            dialog,
            fields,
            names,
            template: OFFERED,
            disk: true,
            armed: None,
            pointer: Point::ORIGIN,
        }
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let column = self.fields.slot_column(band, scale, theme);
        let content = self.fields.measured_height(band, column, scale, theme);
        (
            width,
            self.dialog.height_for_content(content, width, scale, theme),
        )
    }

    /// The client, and where the fields are laid out in it: a choice list
    /// may hang anywhere in the client, over the actions included, since it
    /// is modal.
    fn layout(&self, scale: Scale, theme: &Theme) -> (Rect, FieldLayout) {
        let (width, height) = self.extent(scale, theme);
        let bounds = Rect::new(0, 0, width, height);
        let content = self
            .dialog
            .content_rect(bounds, scale, theme)
            .unwrap_or(Rect::EMPTY);
        (bounds, self.fields.layout(content, bounds, scale, theme))
    }

    /// Paint the whole client.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        let (bounds, fields) = self.layout(scale, theme);
        self.dialog.render(surface, bounds, scale, theme);
        self.fields.render(surface, fields, scale, theme);
        self.fields
            .render_popup(surface, fields.popup, scale, theme);
    }

    fn listing(&self) -> bool {
        self.fields.rows().iter().any(FieldRow::popup_open)
    }

    /// Take `event` at monotonic `at_ns`: the answer, once there is one, and
    /// what was redrawn in `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        let (bounds, fields) = self.layout(scale, theme);
        if let Some(stroke) = Keystroke::pressed(*event, at_ns) {
            return self.on_key(stroke, fields, (scale, theme), damage);
        }
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let listing = self.listing();
        if matches!(event, InputEvent::PointerPressed { .. }) {
            // A press that takes an open list away is not also a press on
            // whatever the list hung over.
            self.armed = Some(if listing || fields.bounds.contains(self.pointer) {
                Part::Fields
            } else {
                Part::Actions
            });
        }
        let mut answer = None;
        if self.armed != Some(Part::Actions) {
            if let Some(action) = self.fields.on_pointer(event, fields, scale, theme, damage) {
                self.take(&action, damage, bounds);
            }
        }
        if self.armed != Some(Part::Fields) && !listing {
            if let Some(DialogAction::ActionActivated { index }) =
                self.dialog.on_pointer(event, bounds, scale, theme, damage)
            {
                answer = Some(self.answer(index));
            }
        }
        if matches!(
            event,
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
        ) {
            self.armed = None;
        }
        answer
    }

    fn on_key(
        &mut self,
        stroke: Keystroke,
        fields: FieldLayout,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        let listing = self.listing();
        if stroke.key == Key::Named(NamedKey::Escape) && !listing {
            return Some(Answer::Cancel);
        }
        if let Some(action) = self.fields.on_key(stroke, fields, scale, theme, damage) {
            self.take(&action, damage, fields.bounds);
            return None;
        }
        if listing {
            return None;
        }
        if let Some(DialogAction::ActionActivated { index }) = self.dialog.on_key(stroke.key) {
            return Some(self.answer(index));
        }
        (stroke.key == Key::Named(NamedKey::Enter)).then(|| self.answer(1))
    }

    /// The name the field holds.
    fn typed(&self) -> &str {
        match self.fields.rows().get(NAME).map(FieldRow::control) {
            Some(FieldControl::Text(field)) => field.text(),
            _ => "",
        }
    }

    fn take(&mut self, action: &FieldGroupAction, damage: &mut Region, whole: Rect) {
        let FieldAction::Selected { index } = action.action else {
            return;
        };
        match action.row {
            MODEL => {
                let was = self.template;
                self.template = index.min(TEMPLATES.len() - 1);
                if self.typed() == self.names[was] {
                    let offered = self.names[self.template].clone();
                    if let Some(FieldControl::Text(field)) = self
                        .fields
                        .rows_mut()
                        .get_mut(NAME)
                        .map(FieldRow::control_mut)
                    {
                        field.set_text(offered);
                    }
                }
                if let Some(row) = self.fields.rows_mut().get_mut(DISK) {
                    *row = disk_row(self.template, self.disk);
                }
            }
            DISK if takes_disk(self.template) => self.disk = index == 0,
            _ => return,
        }
        let description = about(self.template, self.disk);
        if let Some(row) = self.fields.rows_mut().get_mut(MODEL) {
            row.set_description(Some(description));
        }
        damage.add(whole);
    }

    fn answer(&self, index: usize) -> Answer {
        if index == 0 {
            return Answer::Cancel;
        }
        let typed = self.typed().trim();
        let name = if typed.is_empty() {
            self.names[self.template].clone()
        } else {
            let mut name = String::from(typed);
            name.truncate(name.floor_char_boundary(MAX_NAME));
            name
        };
        Answer::Create {
            template: TEMPLATES[self.template].0,
            name,
            disk: self.disk && takes_disk(self.template),
        }
    }
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
