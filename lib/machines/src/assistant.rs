//! The new-machine assistant: a dialog window's client offering an
//! application's models, the name the one chosen is given, and the further
//! choices a machine of that model is made with, a row each.

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

/// The rows, in order: the name, the model, then the model's choices.
const NAME: usize = 0;
const MODEL: usize = 1;
const CHOICES: usize = 2;

/// A model the assistant offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    /// What the model is called.
    pub label: &'static str,
    /// The further choices a machine of it is made with, a row each.
    pub choices: &'static [Choice],
}

/// A further choice a model makes.
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
    /// Make a machine of offer `model` called `name`, its further choices
    /// those chosen.
    Create {
        /// The offer chosen.
        model: usize,
        /// What it is called.
        name: String,
        /// Each of the model's further choices, an index of its row's
        /// choices, none where the row is fixed.
        choices: Vec<Option<usize>>,
    },
}

/// What a machine of offer `model` with further `choices` is, in a line.
pub type Describe = fn(usize, &[Option<usize>]) -> String;

/// The assistant's state.
#[derive(Debug)]
pub struct Assistant {
    form: FormDialog,
    offers: Vec<Offer>,
    /// The name each offer is offered under.
    names: Vec<String>,
    model: usize,
    /// Each choice made, by its row's label: kept across the models that
    /// offer a row so labelled.
    made: Vec<(&'static str, usize)>,
    describe: Describe,
}

/// What `choice` makes, `made` holding the choices made, if it makes
/// anything: the first of its choices until one is made.
fn chosen(choice: &Choice, made: &[(&'static str, usize)]) -> Option<usize> {
    let Choice::Offered { label, choices } = choice else {
        return None;
    };
    let index = made
        .iter()
        .find(|(row, _)| row == label)
        .map_or(0, |&(_, index)| index);
    Some(index.min(choices.len().saturating_sub(1)))
}

/// The row of `choice`, `made` holding the choices made.
fn choice_row(choice: &Choice, made: &[(&'static str, usize)]) -> FieldRow {
    match *choice {
        Choice::Offered { label, choices } => {
            FieldRow::choice(label, choices, chosen(choice, made).unwrap_or(0))
        }
        Choice::Fixed { label, shown, why } => {
            let mut row = FieldRow::choice(label, &[shown], 0).with_description(why);
            row.set_state(ControlState::disabled());
            row
        }
    }
}

/// What each of `offer`'s choices makes, `made` holding the choices made.
fn made_of(offer: &Offer, made: &[(&'static str, usize)]) -> Vec<Option<usize>> {
    offer
        .choices
        .iter()
        .map(|choice| chosen(choice, made))
        .collect()
}

impl Assistant {
    /// The assistant offering `offers`, `offered` among them first; `names`
    /// gives the name each is offered under, and `describe` what a machine
    /// of each is.
    #[must_use]
    pub fn new(offers: Vec<Offer>, names: Vec<String>, offered: usize, describe: Describe) -> Self {
        let model = offered.min(offers.len().saturating_sub(1));
        let made = Vec::new();
        let name = names.get(model).map_or("", String::as_str);
        let column = FieldColumn::new(vec![group(&offers, model, &made, name, describe)]);
        let dialog = Dialog::new(TITLE).in_window().with_actions(vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from("Create")),
                ControlRole::Recommended,
            ),
        ]);
        Self {
            form: FormDialog::new(dialog, column, WIDTH),
            offers,
            names,
            model,
            made,
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
                let typed = self.typed();
                let name = if typed == self.names.get(was).map_or("", String::as_str) {
                    self.names.get(self.model).map_or("", String::as_str)
                } else {
                    typed
                };
                let rebuilt = group(&self.offers, self.model, &self.made, name, self.describe);
                self.form.column_mut().replace(vec![rebuilt]);
            }
            row => {
                let offered = row
                    .checked_sub(CHOICES)
                    .and_then(|at| self.offers.get(self.model)?.choices.get(at));
                let Some(&Choice::Offered { label, .. }) = offered else {
                    return;
                };
                match self.made.iter_mut().find(|(row, _)| *row == label) {
                    Some((_, made)) => *made = index,
                    None => self.made.push((label, index)),
                }
                let description = (self.describe)(self.model, &self.chosen());
                if let Some(row) = self.row_mut(MODEL) {
                    row.set_description(Some(description));
                }
            }
        }
        let (bounds, _) = self.form.place(scale, theme);
        damage.add(bounds);
    }

    /// What each of the chosen model's choices makes.
    fn chosen(&self) -> Vec<Option<usize>> {
        self.offers
            .get(self.model)
            .map_or_else(Vec::new, |offer| made_of(offer, &self.made))
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
            choices: self.chosen(),
        }
    }
}

/// The machine's rows for offer `model` among `offers`, its choices those
/// `made`, the name field holding `name`.
fn group(
    offers: &[Offer],
    model: usize,
    made: &[(&'static str, usize)],
    name: &str,
    describe: Describe,
) -> FieldGroup {
    let labels: Vec<&str> = offers.iter().map(|offer| offer.label).collect();
    let name = TextField::new().with_text(name).with_max_len(MAX_NAME);
    let mut model_row = FieldRow::choice("Model", &labels, model);
    if let Some(offer) = offers.get(model) {
        model_row = model_row.with_description(describe(model, &made_of(offer, made)));
    }
    let mut rows = vec![FieldRow::new("Name", FieldControl::Text(name)), model_row];
    if let Some(offer) = offers.get(model) {
        rows.extend(offer.choices.iter().map(|choice| choice_row(choice, made)));
    }
    FieldGroup::new("MACHINE", rows)
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
