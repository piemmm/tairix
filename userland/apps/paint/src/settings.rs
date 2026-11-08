//! Paint's settings window: its categories, and the rows each shows of the
//! preferences, on the document host's shared settings window.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_colour::Rgb;
use tairix_controls::{
    Button, ColourField, ColumnAction, ControlState, FieldAction, FieldControl, FieldGroup,
    FieldRow, NumberField, Tab,
};
use tairix_icon::IconKind;
use tairix_window::docapp::settings::{self, SettingsWindow};

use crate::canvas::MAX_SIDE;
use crate::dialog::side_row;
use crate::document::colours_for;
use crate::layout::Faces;
use crate::pane::{Arrangement, PaneKind, Side};
use crate::preferences::{
    GridStyle, OpenAt, Preferences, Shades, Surround, CHECKER_SIDES, LEAST_GRID_OPACITY,
    MOST_GRID_SPACING, PIXEL_GRID_ZOOMS,
};
use crate::save::SaveFormat;
use crate::tool::Tool;

/// Paint's settings window.
pub type PaintSettings = SettingsWindow<Preferences, Faces>;

/// The zooms, in percent, the pixel grid may be asked to show from, after
/// *Never*.
const PIXEL_GRID_FROM: [u32; 11] = [200, 300, 400, 600, 800, 1200, 1600, 2400, 3200, 4800, 6400];

/// The checker shades a chosen pair starts from, and shows while the
/// theme's are used.
const STARTING_SHADES: (Rgb, Rgb) = (Rgb::new(0x9a, 0x9a, 0x9a), Rgb::new(0xc8, 0xc8, 0xc8));

/// The colour a chosen surround starts from, and shows while the theme's is
/// used.
const STARTING_SURROUND: Rgb = Rgb::new(0x80, 0x80, 0x80);

/// A group of settings, as the sidebar lists them.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Category {
    /// The tool a window starts with, and what a picture opens at.
    #[default]
    General,
    /// What *New picture* offers to start.
    NewPicture,
    /// The grid and the pixel grid.
    Grid,
    /// The checkerboard and what surrounds the picture.
    Canvas,
    /// The panes a new window opens with.
    Panes,
}

impl Category {
    /// Every category, in the sidebar's order.
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::NewPicture,
        Self::Grid,
        Self::Canvas,
        Self::Panes,
    ];

    /// The category at `index` of the sidebar.
    #[must_use]
    pub fn at(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }

    /// Where it stands in the sidebar.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&at| at == self).unwrap_or(0)
    }

    /// What the sidebar calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::NewPicture => "New picture",
            Self::Grid => "Grid",
            Self::Canvas => "Canvas",
            Self::Panes => "Panes",
        }
    }

    const fn icon(self) -> IconKind {
        match self {
            Self::General => IconKind::Settings,
            Self::NewPicture => IconKind::Image,
            Self::Grid => IconKind::PixelGrid,
            Self::Canvas => IconKind::Wallpaper,
            Self::Panes => IconKind::Display,
        }
    }
}

/// What a settings row asks only Paint for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ask {
    /// Take the panes of the window last worked in as the ones a new window
    /// opens with.
    TakePanes,
}

/// The rows of each category's groups, in order.
mod row {
    pub const TOOL: usize = 0;
    pub const OPEN_AT: usize = 1;

    pub const WIDTH: usize = 0;
    pub const HEIGHT: usize = 1;
    pub const FORMAT: usize = 2;
    pub const COLOURS: usize = 3;
    pub const TRANSPARENT: usize = 4;

    pub const ACROSS: usize = 0;
    pub const DOWN: usize = 1;
    pub const OFFSET_ACROSS: usize = 2;
    pub const OFFSET_DOWN: usize = 3;
    pub const COLOUR: usize = 4;
    pub const OPACITY: usize = 5;
    pub const STYLE: usize = 6;
    pub const SHOWN: usize = 7;
    pub const SNAP: usize = 8;
    pub const PIXELS_FROM: usize = 0;

    pub const SQUARES: usize = 0;
    pub const SHADES: usize = 1;
    pub const DARK: usize = 2;
    pub const LIGHT: usize = 3;
    pub const AROUND: usize = 0;
    pub const SURROUND: usize = 1;

    pub const TAKE: usize = 0;
    pub const RESET: usize = 1;
}

impl settings::Preferences for Preferences {
    const TITLE: &'static str = "Paint settings";
    type Ask = Ask;

    fn categories() -> Vec<Tab> {
        Category::ALL
            .iter()
            .map(|category| Tab::new(category.label()).with_icon(category.icon()))
            .collect()
    }

    fn groups(&self, category: usize) -> Vec<FieldGroup> {
        match Category::at(category) {
            Category::General => vec![general(self)],
            Category::NewPicture => vec![new_picture(self)],
            Category::Grid => grid(self),
            Category::Canvas => canvas(self),
            Category::Panes => vec![panes(self)],
        }
    }

    fn act(&mut self, category: usize, acted: &ColumnAction) -> Option<Ask> {
        let (group, row, action) = (acted.group, acted.action.row, &acted.action.action);
        match Category::at(category) {
            Category::General => edit_general(self, row, action),
            Category::NewPicture => edit_new_picture(self, row, action),
            Category::Grid => edit_grid(self, (group, row), action),
            Category::Canvas => edit_canvas(self, (group, row), action),
            Category::Panes => match (row, action) {
                (row::TAKE, FieldAction::Activated) => return Some(Ask::TakePanes),
                (row::RESET, FieldAction::Activated) => self.panes = Arrangement::default(),
                _ => {}
            },
        }
        None
    }

    fn reshapes(&self, was: &Self, category: usize) -> bool {
        match Category::at(category) {
            // The format decides the colours offered and whether a transparent
            // background may be; a typed size or a pick among those is already
            // what its own control shows.
            Category::NewPicture => was.format != self.format,
            Category::Canvas => {
                (was.shades == Shades::Theme) != (self.shades == Shades::Theme)
                    || (was.surround == Surround::Theme) != (self.surround == Surround::Theme)
            }
            Category::Panes => was.panes != self.panes,
            Category::General | Category::Grid => false,
        }
    }
}

/// A row holding a whole number from `least` to `most`.
fn number_row(label: &str, value: u32, (least, most): (u32, u32)) -> FieldRow {
    let to = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    FieldRow::new(
        label,
        FieldControl::Number(NumberField::new(to(value), to(least), to(most))),
    )
}

/// A row holding a colour, disabled where `enabled` is not.
fn colour_row(label: &str, colour: Rgb, enabled: bool) -> FieldRow {
    let row = FieldRow::new(
        label,
        FieldControl::Colour(ColourField::new(colour.opaque(), false)),
    );
    if enabled {
        row
    } else {
        row.with_state(ControlState::disabled())
    }
}

/// A row offering a command.
fn command_row(label: &str, command: &str) -> FieldRow {
    FieldRow::new(label, FieldControl::Button(Button::labelled(command)))
}

/// Where `value` stands in `all`, or the first entry when it is none of them.
fn index_in<T: PartialEq>(all: &[T], value: &T) -> usize {
    all.iter().position(|at| at == value).unwrap_or(0)
}

/// The General category: the starting tool and how pictures open.
fn general(record: &Preferences) -> FieldGroup {
    let tools: Vec<&str> = Tool::ALL.iter().map(|tool| tool.name()).collect();
    let opens: Vec<&str> = OpenAt::ALL.iter().map(|open| open.label()).collect();
    FieldGroup::new(
        "NEW WINDOWS",
        vec![
            FieldRow::choice("Starting tool", &tools, index_in(&Tool::ALL, &record.tool)),
            FieldRow::choice(
                "Open pictures",
                &opens,
                index_in(&OpenAt::ALL, &record.open_at),
            ),
        ],
    )
}

/// The New picture category: its size, format, colours and background, the
/// background held back where the format keeps none.
fn new_picture(record: &Preferences) -> FieldGroup {
    let formats: Vec<&str> = SaveFormat::ALL
        .iter()
        .map(|format| format.label())
        .collect();
    let colours: Vec<&str> = colours_for(record.format)
        .map(|colours| colours.label)
        .collect();
    let depth_at = colours_for(record.format)
        .position(|colours| colours.depth == record.new.depth)
        .unwrap_or(0);
    let mut transparent = FieldRow::toggle("Transparent background", record.new.transparent);
    if !record.format.holds_transparency() {
        transparent.set_state(ControlState::disabled());
    }
    FieldGroup::new(
        "NEW PICTURE",
        vec![
            side_row("Width", record.new.size.0),
            side_row("Height", record.new.size.1),
            FieldRow::choice(
                "Format",
                &formats,
                index_in(&SaveFormat::ALL, &record.format),
            ),
            FieldRow::choice("Colours", &colours, depth_at),
            transparent,
        ],
    )
}

/// The Grid category: the spaced grid's spacing, offset, colour, opacity,
/// style and use, then the zoom the pixel grid starts at.
fn grid(record: &Preferences) -> Vec<FieldGroup> {
    let styles: Vec<&str> = GridStyle::ALL.iter().map(|style| style.label()).collect();
    let spelt: Vec<String> = core::iter::once(String::from("Never"))
        .chain(PIXEL_GRID_FROM.iter().map(|zoom| format!("{zoom}%")))
        .collect();
    let zooms: Vec<&str> = spelt.iter().map(String::as_str).collect();
    let from = if record.pixel_grid_from == 0 {
        0
    } else {
        PIXEL_GRID_FROM
            .iter()
            .position(|&zoom| zoom >= record.pixel_grid_from)
            .map_or(PIXEL_GRID_FROM.len(), |at| at + 1)
    };
    let grid = &record.grid;
    vec![
        FieldGroup::new(
            "GRID",
            vec![
                number_row("Across", grid.spacing.0, (1, MOST_GRID_SPACING)),
                number_row("Down", grid.spacing.1, (1, MOST_GRID_SPACING)),
                number_row("Offset across", grid.offset.0, (0, MOST_GRID_SPACING - 1)),
                number_row("Offset down", grid.offset.1, (0, MOST_GRID_SPACING - 1)),
                colour_row("Colour", grid.colour, true),
                number_row("Opacity", grid.opacity / 10, (LEAST_GRID_OPACITY / 10, 100))
                    .with_description("In percent."),
                FieldRow::choice("Style", &styles, index_in(&GridStyle::ALL, &grid.style)),
                FieldRow::toggle("Show in new windows", grid.shown),
                FieldRow::toggle("Snap to the grid", grid.snap),
            ],
        ),
        FieldGroup::new(
            "PIXEL GRID",
            vec![FieldRow::choice("Show from", &zooms, from)],
        )
        .with_footnote("Lines between pixels, from this zoom in."),
    ]
}

/// The Canvas category: the checker's squares and shades, then what lies
/// around the picture, a chosen colour's row held back while the theme's is
/// used.
fn canvas(record: &Preferences) -> Vec<FieldGroup> {
    let (dark, light) = match record.shades {
        Shades::Theme => STARTING_SHADES,
        Shades::Chosen(dark, light) => (dark, light),
    };
    let around = match record.surround {
        Surround::Theme => STARTING_SURROUND,
        Surround::Chosen(colour) => colour,
    };
    let shades = record.shades != Shades::Theme;
    let surround = record.surround != Surround::Theme;
    let theirs = ["The theme's", "Chosen"];
    vec![
        FieldGroup::new(
            "CHECKERBOARD",
            vec![
                number_row("Square size", record.checker_side, CHECKER_SIDES)
                    .with_description("In logical pixels."),
                FieldRow::choice("Shades", &theirs, usize::from(shades)),
                colour_row("Dark", dark, shades),
                colour_row("Light", light, shades),
            ],
        ),
        FieldGroup::new(
            "AROUND THE PICTURE",
            vec![
                FieldRow::choice("Colour from", &theirs, usize::from(surround)),
                colour_row("Colour", around, surround),
            ],
        ),
    ]
}

/// The Panes category: where each pane of a new window stands, and the two
/// ways to change it.
fn panes(record: &Preferences) -> FieldGroup {
    FieldGroup::new(
        "NEW WINDOWS",
        vec![
            command_row("From the front window", "Use the front window's"),
            command_row("As Paint lays them out", "Reset"),
        ],
    )
    .with_footnote(described(&record.panes))
}

/// `value` as a count, none where it is negative.
fn unsigned(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// The number a number row's action holds.
const fn number_of(action: &FieldAction) -> Option<i32> {
    match action {
        FieldAction::Number(number) => Some(number.value()),
        _ => None,
    }
}

/// The colour a colour row's action holds, as an opaque colour.
const fn colour_of(action: &FieldAction) -> Option<Rgb> {
    match action {
        FieldAction::Colour(colour) => Some(colour.colour().without_alpha()),
        _ => None,
    }
}

/// The choice a choice row's action made.
const fn chosen(action: &FieldAction) -> Option<usize> {
    match action {
        FieldAction::Selected { index } => Some(*index),
        _ => None,
    }
}

/// The value a switch row's action sets.
const fn switched(action: &FieldAction) -> Option<bool> {
    match action {
        FieldAction::Set { on } => Some(*on),
        _ => None,
    }
}

/// Carry `action` of General row `row` into `record`.
fn edit_general(record: &mut Preferences, row: usize, action: &FieldAction) {
    let Some(index) = chosen(action) else {
        return;
    };
    match row {
        row::TOOL => record.tool = Tool::ALL.get(index).copied().unwrap_or(record.tool),
        row::OPEN_AT => record.open_at = OpenAt::ALL.get(index).copied().unwrap_or(record.open_at),
        _ => {}
    }
}

/// Carry `action` of New picture row `row` into `record`.
fn edit_new_picture(record: &mut Preferences, row: usize, action: &FieldAction) {
    match row {
        row::WIDTH | row::HEIGHT => {
            if let Some(value) = number_of(action) {
                let side = unsigned(value).clamp(1, MAX_SIDE);
                if row == row::WIDTH {
                    record.new.size.0 = side;
                } else {
                    record.new.size.1 = side;
                }
            }
        }
        row::FORMAT => {
            if let Some(index) = chosen(action) {
                record.format = SaveFormat::ALL.get(index).copied().unwrap_or(record.format);
            }
        }
        row::COLOURS => {
            if let Some(colours) =
                chosen(action).and_then(|index| colours_for(record.format).nth(index))
            {
                record.new.depth = colours.depth;
            }
        }
        row::TRANSPARENT => {
            if let Some(on) = switched(action) {
                record.new.transparent = on;
            }
        }
        _ => {}
    }
}

/// Carry `action` of the Grid category's row `row` of group `group` into
/// `record`.
fn edit_grid(record: &mut Preferences, (group, row): (usize, usize), action: &FieldAction) {
    let grid = &mut record.grid;
    if group == 1 {
        if let (row::PIXELS_FROM, Some(index)) = (row, chosen(action)) {
            record.pixel_grid_from = index
                .checked_sub(1)
                .and_then(|at| PIXEL_GRID_FROM.get(at).copied())
                .unwrap_or(0)
                .clamp(0, PIXEL_GRID_ZOOMS.1);
        }
        return;
    }
    let number = number_of(action).map(unsigned);
    match row {
        row::ACROSS => {
            grid.spacing.0 = number.map_or(grid.spacing.0, |n| n.clamp(1, MOST_GRID_SPACING));
        }
        row::DOWN => {
            grid.spacing.1 = number.map_or(grid.spacing.1, |n| n.clamp(1, MOST_GRID_SPACING));
        }
        row::OFFSET_ACROSS => {
            grid.offset.0 = number.map_or(grid.offset.0, |n| n.min(MOST_GRID_SPACING - 1));
        }
        row::OFFSET_DOWN => {
            grid.offset.1 = number.map_or(grid.offset.1, |n| n.min(MOST_GRID_SPACING - 1));
        }
        row::COLOUR => grid.colour = colour_of(action).unwrap_or(grid.colour),
        row::OPACITY => {
            if let Some(percent) = number {
                grid.opacity = (percent * 10).clamp(LEAST_GRID_OPACITY, 1000);
            }
        }
        row::STYLE => {
            if let Some(index) = chosen(action) {
                grid.style = GridStyle::ALL.get(index).copied().unwrap_or(grid.style);
            }
        }
        row::SHOWN => grid.shown = switched(action).unwrap_or(grid.shown),
        row::SNAP => grid.snap = switched(action).unwrap_or(grid.snap),
        _ => {}
    }
}

/// Carry `action` of the Canvas category's row `row` of group `group` into
/// `record`: choosing a colour of one's own starts from the colour shown for
/// the theme's.
fn edit_canvas(record: &mut Preferences, (group, row): (usize, usize), action: &FieldAction) {
    match (group, row) {
        (0, row::SQUARES) => {
            if let Some(value) = number_of(action) {
                record.checker_side = unsigned(value).clamp(CHECKER_SIDES.0, CHECKER_SIDES.1);
            }
        }
        (0, row::SHADES) => {
            if let Some(index) = chosen(action) {
                record.shades = match (index, record.shades) {
                    (0, _) => Shades::Theme,
                    (_, Shades::Theme) => Shades::Chosen(STARTING_SHADES.0, STARTING_SHADES.1),
                    (_, chosen) => chosen,
                };
            }
        }
        (0, row::DARK | row::LIGHT) => {
            if let (Some(colour), Shades::Chosen(dark, light)) =
                (colour_of(action), &mut record.shades)
            {
                *if row == row::DARK { dark } else { light } = colour;
            }
        }
        (1, row::AROUND) => {
            if let Some(index) = chosen(action) {
                record.surround = match (index, record.surround) {
                    (0, _) => Surround::Theme,
                    (_, Surround::Theme) => Surround::Chosen(STARTING_SURROUND),
                    (_, chosen) => chosen,
                };
            }
        }
        (1, row::SURROUND) => {
            if let (Some(colour), Surround::Chosen(held)) =
                (colour_of(action), &mut record.surround)
            {
                *held = colour;
            }
        }
        _ => {}
    }
}

/// `panes` in words.
fn described(panes: &Arrangement) -> String {
    let mut words = String::new();
    for side in Side::BOTH {
        let names: Vec<&str> = panes
            .docked(side)
            .iter()
            .map(|docked| docked.kind.title())
            .collect();
        if names.is_empty() {
            continue;
        }
        if !words.is_empty() {
            words.push_str("; ");
        }
        let edge = match side {
            Side::Left => "on the left",
            Side::Right => "on the right",
        };
        let _ = write!(words, "{} {edge}", names.join(", "));
    }
    let strip: Vec<&str> = panes
        .in_strip()
        .then_some(PaneKind::Palette.title())
        .into_iter()
        .collect();
    let floating: Vec<&str> = panes.floating().map(PaneKind::title).collect();
    let hidden: Vec<&str> = PaneKind::ALL
        .iter()
        .filter(|&&kind| !panes.shows(kind))
        .map(|kind| kind.title())
        .collect();
    for (names, state) in [
        (strip, "beneath the picture"),
        (floating, "floating"),
        (hidden, "hidden"),
    ] {
        if names.is_empty() {
            continue;
        }
        if !words.is_empty() {
            words.push_str("; ");
        }
        let _ = write!(words, "{} {state}", names.join(", "));
    }
    words
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
