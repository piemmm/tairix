//! The questions a window asks: a form of settings in a dialog, answered or
//! turned down.
//!
//! A form's rows are the shared form-field family's; what each form means is
//! read back from them only when it is answered, and a value that does not
//! parse is stated on the dialog itself, which stays open to be put right.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    Button, ButtonContent, ControlRole, Dialog, DialogAction, FieldAction, FieldControl,
    FieldGroup, FieldGroupAction, FieldLayout, FieldRow, Keystroke, NumberField, Slider,
    TextAction, TextField, ValidationState,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{IndexDepth, SpriteName, TiffCompression};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::canvas::{admissible, MAX_PIXELS, MAX_SIDE};
use crate::document::{colours_for, NewPicture, COLOURS, NAME_REFUSAL};
use crate::save::{Loss, SaveFormat, SaveSettings};
use crate::transform::{Anchor, PaletteChoice};

/// A form's width, in logical pixels.
const FORM_WIDTH: u32 = 460;

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

/// The compressions a TIFF may be written under, as their choices read.
const COMPRESSIONS: [(TiffCompression, &str); 4] = [
    (TiffCompression::None, "None"),
    (TiffCompression::Lzw, "LZW"),
    (TiffCompression::Deflate, "Deflate"),
    (TiffCompression::PackBits, "PackBits"),
];

/// What a form asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Purpose {
    /// A new picture in a new window.
    NewPicture,
    /// A new page in this document.
    NewPage,
    /// A new sprite in this document.
    NewSprite,
    /// Stretch or shrink the picture.
    Scale,
    /// Change the canvas's size.
    Canvas,
    /// Store the picture at another depth.
    Convert,
    /// The format to save as and its settings, before the picker asks where;
    /// the window closes once it is saved when `then_close`.
    SaveAs {
        /// Close the window once saved.
        then_close: bool,
    },
}

/// What a Save As sheet offers: each format the document can be written as,
/// with what that format would not keep of it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaveChoices {
    /// The formats, and the losses each states.
    pub formats: Vec<(SaveFormat, Vec<Loss>)>,
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
    /// The formats a format row offers, in its order, with what each would
    /// not keep; and the one chosen.
    formats: Vec<(SaveFormat, Vec<Loss>)>,
    format: SaveFormat,
    /// The settings a Save As sheet is answered with, kept as its rows come
    /// and go with the format.
    settings: SaveSettings,
}

fn text_row(label: &str, text: &str, len: usize) -> FieldRow {
    FieldRow::new(
        label,
        FieldControl::Text(TextField::new().with_text(text).with_max_len(len)),
    )
}

/// A row holding a picture's side, in whole pixels from one to the most a
/// picture may have.
pub(crate) fn side_row(label: &str, side: u32) -> FieldRow {
    let most = i32::try_from(MAX_SIDE).unwrap_or(i32::MAX);
    let side = i32::try_from(side).unwrap_or(most);
    FieldRow::new(label, FieldControl::Number(NumberField::new(side, 1, most)))
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
            formats: Vec::new(),
            format: SaveFormat::Png,
            settings: SaveSettings::default(),
        }
    }

    /// The form for a new picture, starting at `picture` to be saved as
    /// `format`: the format, and the colours and background that format
    /// holds.
    #[must_use]
    pub fn new_picture(picture: NewPicture, format: SaveFormat) -> Self {
        let mut form = Self::new(Purpose::NewPicture, "New picture", "Create", Vec::new());
        form.formats = SaveFormat::ALL
            .into_iter()
            .map(|format| (format, Vec::new()))
            .collect();
        form.format = format;
        form.rebuild(picture.size, picture.depth, picture.transparent);
        form
    }

    /// The form for a new page, sized as `size` to start.
    #[must_use]
    pub fn new_page(size: (u32, u32)) -> Self {
        Self::new(
            Purpose::NewPage,
            "New page",
            "Create",
            vec![
                side_row("Width", size.0),
                side_row("Height", size.1),
                FieldRow::choice("Colours", &COLOURS.map(|colours| colours.label), 0),
                FieldRow::toggle("Transparent background", false),
            ],
        )
    }

    /// The Save As sheet: the format among `choices`, `format` to start, and
    /// that format's own settings, from `settings`.
    #[must_use]
    pub fn save_as(
        choices: SaveChoices,
        format: SaveFormat,
        settings: SaveSettings,
        then_close: bool,
    ) -> Self {
        let mut form = Self::new(
            Purpose::SaveAs { then_close },
            "Save as",
            "Choose name",
            Vec::new(),
        );
        form.formats = choices.formats;
        form.format = form
            .formats
            .iter()
            .any(|(offered, _)| *offered == format)
            .then_some(format)
            .or_else(|| form.formats.first().map(|(first, _)| *first))
            .unwrap_or(format);
        form.settings = settings;
        form.rebuild_save_as();
        form
    }

    /// The format row's choice labels and the one chosen.
    fn format_row(&self) -> FieldRow {
        let labels: Vec<&str> = self
            .formats
            .iter()
            .map(|(format, _)| format.label())
            .collect();
        let chosen = self
            .formats
            .iter()
            .position(|(format, _)| *format == self.format)
            .unwrap_or(0);
        FieldRow::choice("Format", &labels, chosen)
    }

    /// Lay the new-picture form out again for the format chosen, keeping the
    /// size typed, the colours `depth` names where the format holds them, and
    /// the background where it can be clear.
    fn rebuild(&mut self, (width, height): (u32, u32), depth: Option<IndexDepth>, clear: bool) {
        let format = self.format;
        let chosen = colours_for(format)
            .position(|offered| offered.depth == depth)
            .unwrap_or(0);
        let labels: Vec<&str> = colours_for(format).map(|offered| offered.label).collect();
        let mut rows = vec![
            self.format_row(),
            side_row("Width", width),
            side_row("Height", height),
            FieldRow::choice("Colours", &labels, chosen),
        ];
        if format.holds_transparency() {
            rows.push(FieldRow::toggle("Transparent background", clear));
        }
        self.replace_rows(rows);
    }

    /// Lay the Save As sheet out again for the format chosen: its own
    /// settings, and what it would not keep in the message.
    fn rebuild_save_as(&mut self) {
        let mut rows = vec![self.format_row()];
        match self.format {
            SaveFormat::Jpeg => {
                let quality = self.settings.jpeg_quality;
                rows.push(FieldRow::new(
                    quality_label(quality),
                    FieldControl::Slider(
                        Slider::new(quality_permille(quality))
                            .with_stops(100)
                            .with_steps(11, 101),
                    ),
                ));
            }
            SaveFormat::Gif => {
                rows.push(FieldRow::toggle("Interlaced", self.settings.gif.interlaced));
            }
            SaveFormat::Tiff => {
                let chosen = COMPRESSIONS
                    .iter()
                    .position(|(compression, _)| *compression == self.settings.tiff.compression)
                    .unwrap_or(0);
                rows.push(FieldRow::choice(
                    "Compression",
                    &COMPRESSIONS.map(|(_, label)| label),
                    chosen,
                ));
            }
            SaveFormat::Png | SaveFormat::Bmp | SaveFormat::Sprites | SaveFormat::OpenRaster => {}
        }
        let notes = self
            .formats
            .iter()
            .find(|(format, _)| *format == self.format)
            .map(|(_, losses)| {
                let mut notes = String::new();
                for loss in losses {
                    if !notes.is_empty() {
                        notes.push_str(". ");
                    }
                    notes.push_str(loss.message());
                }
                notes
            })
            .unwrap_or_default();
        let dialog = Dialog::new(self.dialog.title()).with_actions(self.dialog.actions().to_vec());
        self.dialog = if notes.is_empty() {
            dialog
        } else {
            dialog.with_message(notes)
        };
        self.replace_rows(rows);
    }

    /// Put `rows` in the form, the keyboard staying on the row it was on.
    fn replace_rows(&mut self, rows: Vec<FieldRow>) {
        let focus = self.group.focus().filter(|&row| row < rows.len());
        self.group = FieldGroup::new("", rows);
        self.group.adopt_focus(focus.or(Some(0)));
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
                side_row("Width", size.0),
                side_row("Height", size.1),
                FieldRow::choice("Colours", &COLOURS.map(|colours| colours.label), 2),
                FieldRow::choice("Pixel shape", &SHAPES.map(|(_, label)| label), 0),
                FieldRow::toggle("Mask", false),
            ],
        )
    }

    /// The form scaling a picture of `size`; `smooth` is offered only where
    /// the picture can show a colour between two of its own.
    #[must_use]
    pub fn scale(size: (u32, u32), smooth: bool) -> Self {
        let mut rows = vec![
            side_row("Width", size.0),
            side_row("Height", size.1),
            FieldRow::toggle("Keep proportions", true),
        ];
        if smooth {
            rows.push(FieldRow::toggle("Smooth", true));
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
                side_row("Width", size.0),
                side_row("Height", size.1),
                FieldRow::choice("Keep the picture at", &ANCHORS, 4),
            ],
        )
    }

    /// The form converting a picture to another depth.
    #[must_use]
    pub fn convert(current: Option<IndexDepth>) -> Self {
        let selected = COLOURS
            .iter()
            .position(|colours| colours.depth == current)
            .unwrap_or(0);
        Self::new(
            Purpose::Convert,
            "Colours",
            "Convert",
            vec![
                FieldRow::choice("Store as", &COLOURS.map(|colours| colours.label), selected),
                FieldRow::choice("Palette", &PALETTES.map(|(_, label)| label), 1),
                FieldRow::toggle("Dither", true),
            ],
        )
    }

    /// What the form asks for.
    #[must_use]
    pub const fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// What the form is titled.
    #[must_use]
    pub fn title(&self) -> &str {
        self.dialog.title()
    }

    /// Where the form is drawn with its top-left at `origin`: its width, and
    /// as tall as its rows need.
    #[must_use]
    pub fn rect(&self, origin: Point, scale: Scale, theme: &Theme) -> Rect {
        let width = scale.scale_length(FORM_WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let column = self.group.slot_column(band, scale, theme);
        let content = self.group.measured_height(band, column, scale, theme);
        let height = self.dialog.height_for_content(content, width, scale, theme);
        Rect::new(origin.x, origin.y, width, height)
    }

    /// The rows laid out in the form at `origin`, a list one of them opens
    /// placed within the form: its window is all the room it has.
    fn group_layout(&self, origin: Point, scale: Scale, theme: &Theme) -> Option<FieldLayout> {
        let bounds = self.rect(origin, scale, theme);
        let content = self.dialog.content_rect(bounds, scale, theme)?;
        Some(self.group.layout(content, bounds, scale, theme))
    }

    /// Paint the form with its top-left at `origin`.
    pub fn render(&self, surface: &mut Surface, origin: Point, scale: Scale, theme: &Theme) {
        let bounds = self.rect(origin, scale, theme);
        self.dialog.render(surface, bounds, scale, theme);
        if let Some(layout) = self.group_layout(origin, scale, theme) {
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
        origin: Point,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Answer> {
        if let Some(layout) = self.group_layout(origin, scale, theme) {
            let acted = self.group.on_pointer(event, layout, scale, theme, damage);
            if let Some(action) = acted {
                return self.adopt(&action, (origin, scale, theme), damage);
            }
        }
        let bounds = self.rect(origin, scale, theme);
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
        origin: Point,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Answer> {
        let listing = self.group.rows().iter().any(FieldRow::popup_open);
        let layout = self.group_layout(origin, scale, theme)?;
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
        self.adopt(&acted, (origin, scale, theme), damage)
    }

    /// Follow what a row asked for, keeping the rows that restate each other
    /// in step.
    fn adopt(
        &mut self,
        acted: &FieldGroupAction,
        (origin, scale, theme): (Point, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Answer> {
        if let FieldAction::Text(TextAction::Submitted) = acted.action {
            return Some(Answer::Confirmed);
        }
        if let FieldAction::Text(TextAction::Cancelled) = acted.action {
            return Some(Answer::Cancelled);
        }
        // The sheet may change size, so what it covered is repainted too.
        damage.add(self.rect(origin, scale, theme));
        match self.purpose {
            Purpose::Scale => self.follow_proportions(acted),
            Purpose::NewPicture => self.follow_new_picture(acted),
            Purpose::SaveAs { .. } => self.follow_save_as(acted),
            _ => {}
        }
        damage.add(self.rect(origin, scale, theme));
        None
    }

    /// The format chosen at `index` of the format row.
    fn chosen_format(&self, index: usize) -> SaveFormat {
        self.formats
            .get(index)
            .map_or(self.format, |(format, _)| *format)
    }

    fn follow_new_picture(&mut self, acted: &FieldGroupAction) {
        let FieldAction::Selected { index } = acted.action else {
            return;
        };
        if acted.row != 0 || self.chosen_format(index) == self.format {
            return;
        }
        let typed = (self.side(1), self.side(2));
        let depth = self.new_depth();
        let clear = self.on(4);
        self.format = self.chosen_format(index);
        self.rebuild(typed, depth, clear);
    }

    fn follow_save_as(&mut self, acted: &FieldGroupAction) {
        match (acted.row, &acted.action) {
            (0, FieldAction::Selected { index }) => {
                let format = self.chosen_format(*index);
                if format != self.format {
                    self.format = format;
                    self.rebuild_save_as();
                }
            }
            (1, FieldAction::SetValue { permille } | FieldAction::Settled { permille })
                if self.format == SaveFormat::Jpeg =>
            {
                self.settings.jpeg_quality = quality_of(*permille);
                self.relabel(1, quality_label(self.settings.jpeg_quality));
            }
            (1, FieldAction::Set { on }) if self.format == SaveFormat::Gif => {
                self.settings.gif.interlaced = *on;
            }
            (1, FieldAction::Selected { index }) if self.format == SaveFormat::Tiff => {
                if let Some((compression, _)) = COMPRESSIONS.get(*index) {
                    self.settings.tiff.compression = *compression;
                }
            }
            _ => {}
        }
    }

    /// The depth the new-picture form's colours row names.
    fn new_depth(&self) -> Option<IndexDepth> {
        colours_for(self.format)
            .nth(self.choice(3))
            .and_then(|colours| colours.depth)
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
        let FieldAction::Number(action) = acted.action else {
            return;
        };
        if !self.on(2) {
            return;
        }
        let (width, height) = (u64::from(self.proportions.0), u64::from(self.proportions.1));
        let (to, num, den) = match acted.row {
            0 => (1, height, width),
            1 => (0, width, height),
            _ => return,
        };
        let Ok(value) = u64::try_from(action.value()) else {
            return;
        };
        let other = (value * num + den / 2) / den.max(1);
        self.set_side(to, u32::try_from(other.max(1)).unwrap_or(u32::MAX));
    }

    fn text(&self, row: usize) -> &str {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Text(field)) => field.text(),
            _ => "",
        }
    }

    /// Row `row`'s side as it stands, the last held where its typing spells
    /// none.
    fn side(&self, row: usize) -> u32 {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Number(field)) => u32::try_from(field.value()).unwrap_or(1),
            _ => 1,
        }
    }

    /// Row `row`'s side, where what it holds spells one in range.
    fn typed_side(&self, row: usize) -> Option<u32> {
        match self.group.rows().get(row).map(FieldRow::control) {
            Some(FieldControl::Number(field))
                if field.state().validation == ValidationState::Valid =>
            {
                u32::try_from(field.value()).ok()
            }
            _ => None,
        }
    }

    fn set_side(&mut self, row: usize, side: u32) {
        if let Some(FieldControl::Number(field)) = self
            .group
            .rows_mut()
            .get_mut(row)
            .map(FieldRow::control_mut)
        {
            field.set_value(i32::try_from(side).unwrap_or(i32::MAX));
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

    /// State `reason` on the dialog: an answer that could not be taken.
    pub fn refuse(&mut self, reason: &str) {
        self.dialog = self.dialog.clone().with_reason(reason);
    }

    fn size(&self, first: usize) -> Result<(u32, u32), String> {
        match (self.typed_side(first), self.typed_side(first + 1)) {
            (Some(width), Some(height)) if admissible(width, height) => Ok((width, height)),
            _ => Err(alloc::format!(
                "A size is a whole number of pixels from 1 to {MAX_SIDE} on each side, \
                 and at most {MAX_PIXELS} pixels in all"
            )),
        }
    }

    /// The new picture asked for, and the format it is to be saved as.
    ///
    /// # Errors
    ///
    /// The reason, where the size is not one a picture may have.
    pub fn new_picture_answer(&self) -> Result<(NewPicture, SaveFormat), String> {
        let picture = NewPicture {
            size: self.size(1)?,
            depth: self.new_depth(),
            transparent: self.format.holds_transparency() && self.on(4),
        };
        Ok((picture, self.format))
    }

    /// The new page asked for.
    ///
    /// # Errors
    ///
    /// The reason, where the size is not one a picture may have.
    pub fn new_page_answer(&self) -> Result<NewPicture, String> {
        Ok(NewPicture {
            size: self.size(0)?,
            depth: COLOURS[self.choice(2).min(COLOURS.len() - 1)].depth,
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
        let depth = COLOURS[self.choice(3).min(COLOURS.len() - 1)].depth;
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
            COLOURS[self.choice(0).min(COLOURS.len() - 1)].depth,
            PALETTES[self.choice(1).min(PALETTES.len() - 1)].0,
            self.on(2),
        )
    }

    /// The format chosen and the settings it, and every other, is written
    /// with.
    #[must_use]
    pub const fn save_as_answer(&self) -> (SaveFormat, SaveSettings) {
        (self.format, self.settings)
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
