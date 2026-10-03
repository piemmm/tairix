//! The questions a window asks: a form of settings in a dialog, answered or
//! turned down.
//!
//! A form's rows are the shared form-field family's; what each form means is
//! read back from them only when it is answered, and a value that does not
//! parse is stated on the dialog itself, which stays open to be put right.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ComboBox, ControlRole, Dialog, DialogAction, FieldAction, FieldControl,
    FieldGroup, FieldGroupAction, FieldLayout, FieldRow, Keystroke, Slider, TextAction, TextField,
    Toggle,
};
use tairix_geometry::{Rect, Region, Scale};
use tairix_image::{IndexDepth, SpriteName};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::canvas::{admissible, MAX_PIXELS, MAX_SIDE};
use crate::document::{NewPicture, NAME_REFUSAL};
use crate::transform::{Anchor, PaletteChoice};

/// A form's width, in logical pixels.
const FORM_WIDTH: u32 = 460;

/// The longest a number field takes.
const NUMBER_LEN: usize = 6;

/// The depths a picture may be made or converted to, as their choices read.
const DEPTHS: [(Option<IndexDepth>, &str); 5] = [
    (None, "Millions of colours, and transparency"),
    (Some(IndexDepth::Eight), "256 colours"),
    (Some(IndexDepth::Four), "16 colours"),
    (Some(IndexDepth::Two), "4 colours"),
    (Some(IndexDepth::One), "2 colours"),
];

/// The pixel shapes a new sprite may have, as eigen factors across and down.
const SHAPES: [((u8, u8), &str); 3] = [
    ((1, 1), "Square"),
    ((1, 2), "Twice as tall as wide"),
    ((2, 1), "Twice as wide as tall"),
];

const ANCHORS: [&str; 9] = [
    "Top left",
    "Top",
    "Top right",
    "Left",
    "Centre",
    "Right",
    "Bottom left",
    "Bottom",
    "Bottom right",
];

const PALETTES: [(PaletteChoice, &str); 2] = [
    (PaletteChoice::Desktop, "The RISC OS desktop's colours"),
    (
        PaletteChoice::Optimised,
        "The colours that suit the picture",
    ),
];

/// What a form asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Purpose {
    /// A new picture in a new window.
    NewPicture,
    /// A new sprite in this document.
    NewSprite,
    /// Stretch or shrink the picture.
    Scale,
    /// Change the canvas's size.
    Canvas,
    /// Store the picture at another depth.
    Convert,
    /// The JPEG quality to save at.
    Quality,
}

/// How a form was answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// Turned down.
    Cancelled,
    /// Accepted: read what it says.
    Confirmed,
}

/// A form in a dialog.
#[derive(Clone, Debug)]
pub struct Form {
    purpose: Purpose,
    dialog: Dialog,
    group: FieldGroup,
    /// The picture's size, which a scale keeping its proportions follows.
    proportions: (u32, u32),
}

fn text_row(label: &str, text: &str, len: usize) -> FieldRow {
    FieldRow::new(
        label,
        FieldControl::Text(TextField::new().with_text(text).with_max_len(len)),
    )
}

fn choice_row(label: &str, choices: &[&str], selected: usize) -> FieldRow {
    FieldRow::new(
        label,
        FieldControl::Combo(
            ComboBox::new(choices.iter().map(|choice| String::from(*choice)).collect())
                .with_selected(selected),
        ),
    )
}

fn toggle_row(label: &str, on: bool) -> FieldRow {
    FieldRow::new(label, FieldControl::Toggle(Toggle::new(label, on)))
}

impl Form {
    fn new(purpose: Purpose, title: &str, confirm: &str, rows: Vec<FieldRow>) -> Self {
        let dialog = Dialog::new(title).with_actions(vec![
            Button::labelled("Cancel"),
            Button::new(
                ButtonContent::Label(String::from(confirm)),
                ControlRole::Recommended,
            ),
        ]);
        let mut group = FieldGroup::new("", rows);
        group.adopt_focus(Some(0));
        Self {
            purpose,
            dialog,
            group,
            proportions: (1, 1),
        }
    }

    /// The form for a new picture, sized as `size` to start.
    #[must_use]
    pub fn new_picture(size: (u32, u32)) -> Self {
        Self::new(
            Purpose::NewPicture,
            "New picture",
            "Create",
            vec![
                text_row("Width", &size.0.to_string(), NUMBER_LEN),
                text_row("Height", &size.1.to_string(), NUMBER_LEN),
                choice_row("Colours", &DEPTHS.map(|(_, label)| label), 0),
                toggle_row("Transparent background", false),
            ],
        )
    }

    /// The form for a new sprite called `name` to start, sized `size`.
    #[must_use]
    pub fn new_sprite(name: &str, size: (u32, u32)) -> Self {
        Self::new(
            Purpose::NewSprite,
            "New sprite",
            "Create",
            vec![
                text_row("Name", name, SpriteName::MAX_LEN),
                text_row("Width", &size.0.to_string(), NUMBER_LEN),
                text_row("Height", &size.1.to_string(), NUMBER_LEN),
                choice_row("Colours", &DEPTHS.map(|(_, label)| label), 2),
                choice_row("Pixel shape", &SHAPES.map(|(_, label)| label), 0),
                toggle_row("Mask", false),
            ],
        )
    }

    /// The form scaling a picture of `size`; `smooth` is offered only where
    /// the picture can show a colour between two of its own.
    #[must_use]
    pub fn scale(size: (u32, u32), smooth: bool) -> Self {
        let mut rows = vec![
            text_row("Width", &size.0.to_string(), NUMBER_LEN),
            text_row("Height", &size.1.to_string(), NUMBER_LEN),
            toggle_row("Keep proportions", true),
        ];
        if smooth {
            rows.push(toggle_row("Smooth", true));
        }
        let mut form = Self::new(Purpose::Scale, "Resize picture", "Resize", rows);
        form.proportions = (size.0.max(1), size.1.max(1));
        form
    }

    /// The form changing the canvas of a picture of `size`.
    #[must_use]
    pub fn canvas(size: (u32, u32)) -> Self {
        Self::new(
            Purpose::Canvas,
            "Canvas size",
            "Change",
            vec![
                text_row("Width", &size.0.to_string(), NUMBER_LEN),
                text_row("Height", &size.1.to_string(), NUMBER_LEN),
                choice_row("Keep the picture at", &ANCHORS, 4),
            ],
        )
    }

    /// The form converting a picture to another depth.
    #[must_use]
    pub fn convert(current: Option<IndexDepth>) -> Self {
        let selected = DEPTHS
            .iter()
            .position(|(depth, _)| *depth == current)
            .unwrap_or(0);
        Self::new(
            Purpose::Convert,
            "Colours",
            "Convert",
            vec![
                choice_row("Store as", &DEPTHS.map(|(_, label)| label), selected),
                choice_row("Palette", &PALETTES.map(|(_, label)| label), 1),
                toggle_row("Dither", true),
            ],
        )
    }

    /// The form setting the JPEG quality, at `quality` to start.
    #[must_use]
    pub fn quality(quality: u8) -> Self {
        Self::new(
            Purpose::Quality,
            "JPEG quality",
            "Set",
            vec![FieldRow::new(
                quality_label(quality),
                FieldControl::Slider(
                    Slider::new(quality_permille(quality))
                        .with_stops(100)
                        .with_steps(11, 101),
                ),
            )],
        )
    }

    /// What the form asks for.
    #[must_use]
    pub const fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// Where the form is drawn in `window`.
    #[must_use]
    pub fn rect(&self, window: Rect, scale: Scale, theme: &Theme) -> Rect {
        let width = scale.scale_length(FORM_WIDTH).min(window.width);
        let band = Dialog::content_width(width, scale, theme);
        let column = self.group.slot_column(band, scale, theme);
        let content = self.group.measured_height(band, column, scale, theme);
        self.dialog
            .placed_over(window, width, content, scale, theme)
    }

    fn group_layout(&self, window: Rect, scale: Scale, theme: &Theme) -> Option<FieldLayout> {
        let bounds = self.rect(window, scale, theme);
        let content = self.dialog.content_rect(bounds, scale, theme)?;
        Some(self.group.layout(content, window, scale, theme))
    }

    /// Paint the form over `window`.
    pub fn render(&self, surface: &mut Surface, window: Rect, scale: Scale, theme: &Theme) {
        let bounds = self.rect(window, scale, theme);
        self.dialog.render(surface, bounds, scale, theme);
        if let Some(layout) = self.group_layout(window, scale, theme) {
            self.group.render(surface, layout, scale, theme);
            if !layout.popup.is_empty() {
                self.group.render_popup(surface, layout.popup, scale, theme);
            }
        }
    }

    /// Feed one pointer event.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        window: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Answer> {
        if let Some(layout) = self.group_layout(window, scale, theme) {
            let acted = self.group.on_pointer(event, layout, scale, theme, damage);
            if let Some(action) = acted {
                return self.adopt(&action, window, scale, theme, damage);
            }
        }
        let bounds = self.rect(window, scale, theme);
        match self.dialog.on_pointer(event, bounds, scale, theme, damage) {
            Some(DialogAction::ActionActivated { index: 1 }) => Some(Answer::Confirmed),
            Some(DialogAction::ActionActivated { .. }) => Some(Answer::Cancelled),
            None => None,
        }
    }

    /// Feed one key: Escape turns the form down, Enter accepts it unless a
    /// list is open, Tab moves between rows, and the rest goes to the row
    /// with the keyboard.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        window: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Answer> {
        let listing = self.group.rows().iter().any(FieldRow::popup_open);
        let layout = self.group_layout(window, scale, theme)?;
        let stroke = match stroke.key {
            Key::Named(NamedKey::Escape) if !listing => return Some(Answer::Cancelled),
            Key::Named(NamedKey::Enter) if !listing => return Some(Answer::Confirmed),
            Key::Named(NamedKey::Tab) => Keystroke {
                key: Key::Named(if stroke.modifiers.shift {
                    NamedKey::Up
                } else {
                    NamedKey::Down
                }),
                ..stroke
            },
            _ => stroke,
        };
        let acted = self.group.on_key(stroke, layout, scale, theme, damage)?;
        self.adopt(&acted, window, scale, theme, damage)
    }

    /// Follow what a row asked for, keeping the rows that restate each other
    /// in step.
    fn adopt(
        &mut self,
        acted: &FieldGroupAction,
        window: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Answer> {
        if let FieldAction::Text(TextAction::Submitted) = acted.action {
            return Some(Answer::Confirmed);
        }
        if let FieldAction::Text(TextAction::Cancelled) = acted.action {
            return Some(Answer::Cancelled);
        }
        match self.purpose {
            Purpose::Scale => self.follow_proportions(acted),
            Purpose::Quality => {
                if let FieldAction::SetValue { permille } | FieldAction::Settled { permille } =
                    acted.action
                {
                    self.relabel(0, quality_label(quality_of(permille)));
                }
            }
            _ => {}
        }
        damage.add(self.rect(window, scale, theme));
        None
    }

    /// Give row `index` the label `label`, its control and the keyboard
    /// focus as they were.
    fn relabel(&mut self, index: usize, label: String) {
        let focus = self.group.focus();
        if let Some(row) = self.group.rows_mut().get_mut(index) {
            *row = FieldRow::new(label, row.control().clone());
        }
        self.group.adopt_focus(focus);
    }

    fn follow_proportions(&mut self, acted: &FieldGroupAction) {
        if !matches!(acted.action, FieldAction::Text(TextAction::Edited)) || !self.on(2) {
            return;
        }
        let (width, height) = (u64::from(self.proportions.0), u64::from(self.proportions.1));
        let (from, to, num, den) = match acted.row {
            0 => (0, 1, height, width),
            1 => (1, 0, width, height),
            _ => return,
        };
        let Ok(value) = self.text(from).trim().parse::<u64>() else {
            return;
        };
        let other = (value * num + den / 2) / den.max(1);
        self.set_text(to, &other.max(1).to_string());
    }

    fn text(&self, row: usize) -> &str {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Text(field)) => field.text(),
            _ => "",
        }
    }

    fn set_text(&mut self, row: usize, text: &str) {
        if let Some(FieldControl::Text(field)) = self
            .group
            .rows_mut()
            .get_mut(row)
            .map(FieldRow::control_mut)
        {
            field.set_text(text);
        }
    }

    fn choice(&self, row: usize) -> usize {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Combo(combo)) => combo.selected().unwrap_or(0),
            _ => 0,
        }
    }

    fn on(&self, row: usize) -> bool {
        matches!(
            self.group.rows().get(row).map(FieldRow::control),
            Some(FieldControl::Toggle(toggle)) if toggle.is_on()
        )
    }

    fn permille(&self, row: usize) -> u16 {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Slider(slider)) => slider.value(),
            _ => 0,
        }
    }

    /// State `reason` on the dialog: an answer that could not be taken.
    pub fn refuse(&mut self, reason: &str) {
        self.dialog = self.dialog.clone().with_reason(reason);
    }

    fn size(&self, first: usize) -> Result<(u32, u32), String> {
        let side = |row: usize| self.text(row).trim().parse::<u32>().ok();
        match (side(first), side(first + 1)) {
            (Some(width), Some(height)) if admissible(width, height) => Ok((width, height)),
            _ => Err(alloc::format!(
                "A size is a whole number of pixels from 1 to {MAX_SIDE} on each side, \
                 and at most {MAX_PIXELS} pixels in all"
            )),
        }
    }

    /// The new picture asked for.
    ///
    /// # Errors
    ///
    /// The reason, where the size is not one a picture may have.
    pub fn new_picture_answer(&self) -> Result<NewPicture, String> {
        Ok(NewPicture {
            size: self.size(0)?,
            depth: DEPTHS[self.choice(2).min(DEPTHS.len() - 1)].0,
            transparent: self.on(3),
        })
    }

    /// A new sprite's name, size, depth, pixel shape and whether it has a
    /// mask.
    ///
    /// # Errors
    ///
    /// The reason, where the name or the size will not do.
    pub fn new_sprite_answer(&self) -> Result<NewSprite, String> {
        let name =
            SpriteName::new(self.text(0).trim()).ok_or_else(|| String::from(NAME_REFUSAL))?;
        let size = self.size(1)?;
        let depth = DEPTHS[self.choice(3).min(DEPTHS.len() - 1)].0;
        let eig = SHAPES[self.choice(4).min(SHAPES.len() - 1)].0;
        Ok(NewSprite {
            name,
            size,
            depth,
            eig,
            masked: self.on(5),
        })
    }

    /// A scale's size, and whether to filter.
    ///
    /// # Errors
    ///
    /// The reason, where the size is not one a picture may have.
    pub fn scale_answer(&self) -> Result<((u32, u32), bool), String> {
        Ok((self.size(0)?, self.on(3)))
    }

    /// A canvas's size and where the picture stays.
    ///
    /// # Errors
    ///
    /// The reason, where the size is not one a picture may have.
    pub fn canvas_answer(&self) -> Result<((u32, u32), Anchor), String> {
        let anchor = Anchor::ALL[self.choice(2).min(Anchor::ALL.len() - 1)];
        Ok((self.size(0)?, anchor))
    }

    /// A conversion's depth (`None` for colour), palette, and whether to
    /// dither.
    #[must_use]
    pub fn convert_answer(&self) -> (Option<IndexDepth>, PaletteChoice, bool) {
        (
            DEPTHS[self.choice(0).min(DEPTHS.len() - 1)].0,
            PALETTES[self.choice(1).min(PALETTES.len() - 1)].0,
            self.on(2),
        )
    }

    /// The JPEG quality chosen.
    #[must_use]
    pub fn quality_answer(&self) -> u8 {
        quality_of(self.permille(0))
    }
}

/// What a new sprite is to be.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NewSprite {
    /// Its name.
    pub name: SpriteName,
    /// Its size.
    pub size: (u32, u32),
    /// Its depth, `None` for colour.
    pub depth: Option<IndexDepth>,
    /// Its pixels' eigen factors.
    pub eig: (u8, u8),
    /// Whether it has a mask.
    pub masked: bool,
}

fn quality_label(quality: u8) -> String {
    alloc::format!("Quality: {quality}")
}

fn quality_permille(quality: u8) -> u16 {
    u16::try_from((u32::from(quality.clamp(1, 100)) - 1) * 1000 / 99).unwrap_or(1000)
}

fn quality_of(permille: u16) -> u8 {
    u8::try_from(1 + (u32::from(permille.min(1000)) * 99 + 500) / 1000).unwrap_or(100)
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
