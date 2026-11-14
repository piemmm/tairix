//! The new-machine assistant: a dialog window's client offering an
//! application's models, the name the one chosen is given, and the one further
//! choice a machine of that model is made with.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ColumnAction, ControlRole, ControlState, Dialog, FieldAction,
    FieldColumn, FieldControl, FieldGroup, FieldRow, FormAction, FormDialog, TextField,
};
use tairix_geometry::{Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::record::MAX_NAME;

/// The dialog's title, which the desktop's frame carries.
pub const TITLE: &str = "New Machine";

/// The window's width, in logical pixels.
const WIDTH: u32 = 560;

/// The rows, in order.
const NAME: usize = 0;
const MODEL: usize = 1;
const CHOICE: usize = 2;

/// A model the assistant offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    /// What the model is called.
    pub label: &'static str,
    /// The one further choice a machine of it is made with.
    pub choice: Choice,
}

/// The one further choice a model makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A row offering these, the first chosen until another is.
    Offered {
        /// The row's label.
        label: &'static str,
        /// What may be chosen.
        choices: &'static [&'static str],
    },
    /// A row that cannot change: what it shows, and why it cannot.
    Fixed {
        /// The row's label.
        label: &'static str,
        /// What it shows.
        shown: &'static str,
        /// Why it cannot change.
        why: &'static str,
    },
}

/// What the assistant answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Nothing is made.
    Cancel,
    /// Make a machine of offer `model` called `name`, its further choice the
    /// one chosen, none where the model's is fixed.
    Create {
        /// The offer chosen.
        model: usize,
        /// What it is called.
        name: String,
        /// The further choice, an index of the model's choices.
        choice: Option<usize>,
    },
}

/// What a machine of offer `model` with further `choice` is, in a line.
pub type Describe = fn(usize, Option<usize>) -> String;

/// The assistant's state.
#[derive(Debug)]
pub struct Assistant {
    form: FormDialog,
    offers: Vec<Offer>,
    /// The name each offer is offered under.
    names: Vec<String>,
    model: usize,
    /// The further choice made, kept across models that offer it.
    choice: usize,
    describe: Describe,
}

/// The further choice offer `offer` makes with `choice` chosen, if it makes
/// one.
fn chosen(offer: &Offer, choice: usize) -> Option<usize> {
    match offer.choice {
        Choice::Offered { choices, .. } => Some(choice.min(choices.len().saturating_sub(1))),
        Choice::Fixed { .. } => None,
    }
}

/// The further choice's row for `offer` with `choice` chosen.
fn choice_row(offer: &Offer, choice: usize) -> FieldRow {
    match offer.choice {
        Choice::Offered { label, choices } => {
            FieldRow::choice(label, choices, chosen(offer, choice).unwrap_or(0))
        }
        Choice::Fixed { label, shown, why } => {
            let mut row = FieldRow::choice(label, &[shown], 0).with_description(why);
            row.set_state(ControlState::disabled());
            row
        }
    }
}

impl Assistant {
    /// The assistant offering `offers`, `offered` among them first; `names`
    /// gives the name each is offered under, and `describe` what a machine
    /// of each is.
    #[must_use]
    pub fn new(offers: Vec<Offer>, names: Vec<String>, offered: usize, describe: Describe) -> Self {
        let model = offered.min(offers.len().saturating_sub(1));
        let labels: Vec<&str> = offers.iter().map(|offer| offer.label).collect();
        let name = TextField::new()
            .with_text(names.get(model).map_or("", String::as_str))
            .with_max_len(MAX_NAME);
        let about = offers
            .get(model)
            .map(|offer| describe(model, chosen(offer, 0)));
        let mut model_row = FieldRow::choice("Model", &labels, model);
        if let Some(about) = about {
            model_row = model_row.with_description(about);
        }
        let mut rows = vec![FieldRow::new("Name", FieldControl::Text(name)), model_row];
        if let Some(offer) = offers.get(model) {
            rows.push(choice_row(offer, 0));
        }
        let dialog = Dialog::new(TITLE).in_window().with_actions(vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Create")),
                ControlRole::Recommended,
            ),
        ]);
        let column = FieldColumn::new(vec![FieldGroup::new("MACHINE", rows)]);
        Self {
            form: FormDialog::new(dialog, column, WIDTH),
            offers,
            names,
            model,
            choice: 0,
            describe,
        }
    }

    /// The window's extent: its width, and as tall as what it holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        self.form.extent(scale, theme)
    }

    /// Paint the whole client.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.form.render(surface, scale, theme);
    }

    /// Take `event` at monotonic `at_ns`: the answer, once there is one, and
    /// what was redrawn in `damage`.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        at_ns: u64,
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        match self.form.on_input(event, at_ns, looks, damage)? {
            FormAction::Field(acted) => {
                self.take(&acted, looks, damage);
                None
            }
            FormAction::Action(index) => Some(self.answer(index)),
            FormAction::Dismissed => Some(Answer::Cancel),
        }
    }

    /// The fields, for an owner's test of what they show.
    #[must_use]
    pub fn column(&self) -> &FieldColumn {
        self.form.column()
    }

    /// The machine's row of row `row`.
    fn row(&self, row: usize) -> Option<&FieldRow> {
        self.form.column().groups().first()?.rows().get(row)
    }

    /// The machine's row of row `row`, to change.
    fn row_mut(&mut self, row: usize) -> Option<&mut FieldRow> {
        self.form
            .column_mut()
            .groups_mut()
            .first_mut()?
            .rows_mut()
            .get_mut(row)
    }

    /// The name the field holds.
    fn typed(&self) -> &str {
        match self.row(NAME).map(FieldRow::control) {
            Some(FieldControl::Text(field)) => field.text(),
            _ => "",
        }
    }

    fn take(&mut self, acted: &ColumnAction, (scale, theme): (Scale, &Theme), damage: &mut Region) {
        let FieldAction::Selected { index } = acted.action.action else {
            return;
        };
        match acted.action.row {
            MODEL => {
                let was = self.model;
                self.model = index.min(self.offers.len().saturating_sub(1));
                let offered = self.names.get(self.model).cloned();
                if self.typed() == self.names.get(was).map_or("", String::as_str) {
                    if let Some(offered) = offered {
                        self.set_name(offered);
                    }
                }
                let row = self
                    .offers
                    .get(self.model)
                    .map(|offer| choice_row(offer, self.choice));
                if let (Some(row), Some(held)) = (row, self.row_mut(CHOICE)) {
                    *held = row;
                }
            }
            CHOICE if self.made().is_some() => self.choice = index,
            _ => return,
        }
        let description = (self.describe)(self.model, self.made());
        if let Some(row) = self.row_mut(MODEL) {
            row.set_description(Some(description));
        }
        let (bounds, _) = self.form.place(scale, theme);
        damage.add(bounds);
    }

    /// The further choice the model chosen makes, none where it is fixed.
    fn made(&self) -> Option<usize> {
        self.offers
            .get(self.model)
            .and_then(|offer| chosen(offer, self.choice))
    }

    fn set_name(&mut self, name: String) {
        if let Some(FieldControl::Text(field)) = self.row_mut(NAME).map(FieldRow::control_mut) {
            field.set_text(name);
        }
    }

    fn answer(&self, index: usize) -> Answer {
        if index == 0 {
            return Answer::Cancel;
        }
        let typed = self.typed().trim();
        let name = if typed.is_empty() {
            self.names.get(self.model).cloned().unwrap_or_default()
        } else {
            let mut name = String::from(typed);
            name.truncate(name.floor_char_boundary(MAX_NAME));
            name
        };
        Answer::Create {
            model: self.model,
            name,
            choice: self.made(),
        }
    }
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
