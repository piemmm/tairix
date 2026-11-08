//! Draw's settings window: its categories, and the rows each shows of the
//! preferences, on the document host's shared settings window.

use alloc::vec;
use alloc::vec::Vec;

use tairix_colour::Rgba;
use tairix_controls::{
    ColourField, ColumnAction, ControlState, FieldAction, FieldControl, FieldGroup, FieldRow,
    NumberField, Tab,
};
use tairix_drawing::limits::MAX_SUBDIVISIONS;
use tairix_drawing::{Unit, PAPER};
use tairix_geometry::to_i32;
use tairix_icon::IconKind;
use tairix_window::docapp::settings::{self, SettingsWindow};

use crate::inspector::{hundredths, measure};
use crate::layout::Faces;
use crate::preferences::{
    pixels, Colouring, Preferences, MOST_GRID_SPACING, MOST_LENGTH, PICK_REACH,
};
use crate::tools::Tool;

/// Draw's settings window.
pub type DrawSettings = SettingsWindow<Preferences, Faces>;

/// A group of settings, as the sidebar lists them.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Category {
    /// What a new window starts with.
    #[default]
    General,
    /// The page and grid a new drawing starts with.
    NewDrawing,
    /// The style new objects are drawn in.
    Style,
    /// How the keys and the pointer move things.
    Editing,
}

impl Category {
    /// Every category, in the sidebar's order.
    pub const ALL: [Self; 4] = [Self::General, Self::NewDrawing, Self::Style, Self::Editing];

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
            Self::NewDrawing => "New drawing",
            Self::Style => "Style",
            Self::Editing => "Editing",
        }
    }

    const fn icon(self) -> IconKind {
        match self {
            Self::General => IconKind::Settings,
            Self::NewDrawing => IconKind::Page,
            Self::Style => IconKind::Appearance,
            Self::Editing => IconKind::ToolSelect,
        }
    }
}

/// The rows of each category's groups, in order.
mod row {
    pub const TOOL: usize = 0;
    pub const GRID: usize = 1;
    pub const SNAP: usize = 2;
    pub const OUTLINE: usize = 3;

    pub const PAPER: usize = 0;
    pub const ORIENTATION: usize = 1;
    pub const UNIT: usize = 2;
    pub const SPACING: usize = 0;
    pub const SUBDIVISIONS: usize = 1;

    pub const PAINTED: usize = 0;
    pub const COLOUR: usize = 1;
    pub const WIDTH: usize = 2;

    pub const NUDGE: usize = 0;
    pub const DUPLICATE: usize = 1;
    pub const PICK: usize = 0;
}

/// The groups of the Style category, in order.
const FILL: usize = 0;

impl settings::Preferences for Preferences {
    const TITLE: &'static str = "Draw settings";
    type Ask = core::convert::Infallible;

    fn categories() -> Vec<Tab> {
        Category::ALL
            .iter()
            .map(|category| Tab::new(category.label()).with_icon(category.icon()))
            .collect()
    }

    fn groups(&self, category: usize) -> Vec<FieldGroup> {
        match Category::at(category) {
            Category::General => vec![general(self)],
            Category::NewDrawing => new_drawing(self),
            Category::Style => style(self),
            Category::Editing => editing(self),
        }
    }

    fn act(&mut self, category: usize, acted: &ColumnAction) -> Option<Self::Ask> {
        let at = (acted.group, acted.action.row);
        let action = &acted.action.action;
        match Category::at(category) {
            Category::General => edit_general(self, at.1, action),
            Category::NewDrawing => edit_new_drawing(self, at, action),
            Category::Style => edit_style(self, at, action),
            Category::Editing => edit_editing(self, at, action),
        }
        None
    }

    fn reshapes(&self, was: &Self, category: usize) -> bool {
        match Category::at(category) {
            // The spacing is shown in the unit.
            Category::NewDrawing => was.unit != self.unit,
            // A paint of none holds no colour of its own.
            Category::Style => {
                was.fill.is_some() != self.fill.is_some()
                    || was.stroke.is_some() != self.stroke.is_some()
            }
            Category::General | Category::Editing => false,
        }
    }
}

/// A row holding a length in hundredths, from `least` to `most`.
fn length_row(label: &str, value: u32, (least, most): (u32, u32)) -> FieldRow {
    let to = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    FieldRow::new(
        label,
        FieldControl::Number(measure(to(value), to(least), to(most))),
    )
}

/// The rows painting a new object's fill or stroke: none or a colour, and
/// the colour, held back while there is none.
fn paint_rows(label: &str, colouring: Colouring) -> [FieldRow; 2] {
    let colour = FieldRow::new(
        "Colour",
        FieldControl::Colour(ColourField::new(
            colouring.unwrap_or(Rgba::new(0, 0, 0, 255)),
            true,
        )),
    );
    [
        FieldRow::choice(label, &["None", "Colour"], usize::from(colouring.is_some())),
        if colouring.is_some() {
            colour
        } else {
            colour.with_state(ControlState::disabled())
        },
    ]
}

/// The General category: what a new window starts with.
fn general(record: &Preferences) -> FieldGroup {
    let tools: Vec<&str> = Tool::ALL.iter().map(|tool| tool.name()).collect();
    let tool = Tool::ALL
        .iter()
        .position(|&at| at == record.tool)
        .unwrap_or(0);
    FieldGroup::new(
        "NEW WINDOWS",
        vec![
            FieldRow::choice("Starting tool", &tools, tool),
            FieldRow::toggle("Show the grid", record.show.grid),
            FieldRow::toggle(
                "Snap to guides, the page and the grid",
                record.show.snapping,
            ),
            FieldRow::toggle("Draw outlines only", record.show.outline),
        ],
    )
}

/// The New drawing category: its page, then its grid, measured in its unit.
fn new_drawing(record: &Preferences) -> Vec<FieldGroup> {
    let papers: Vec<&str> = PAPER.iter().map(|paper| paper.name).collect();
    let units: Vec<&str> = Unit::ALL.iter().map(|unit| unit.symbol()).collect();
    let unit = Unit::ALL
        .iter()
        .position(|&at| at == record.unit)
        .unwrap_or(0);
    let in_unit = |hundredths_px: u32| hundredths(pixels(hundredths_px) / record.unit.pixels());
    let spacing = NumberField::new(
        in_unit(record.grid_spacing),
        1,
        in_unit(MOST_GRID_SPACING).max(1),
    )
    .with_decimals(2)
    .with_steps(100, 1000);
    vec![
        FieldGroup::new(
            "PAGE",
            vec![
                FieldRow::choice("Paper", &papers, record.paper),
                FieldRow::choice(
                    "Orientation",
                    &["Portrait", "Landscape"],
                    usize::from(record.landscape),
                ),
                FieldRow::choice("Units", &units, unit),
            ],
        ),
        FieldGroup::new(
            "GRID",
            vec![
                FieldRow::new("Spacing", FieldControl::Number(spacing))
                    .with_description("In the drawing's units."),
                FieldRow::new(
                    "Subdivisions",
                    FieldControl::Number(NumberField::new(
                        to_i32(record.subdivisions),
                        1,
                        to_i32(MAX_SUBDIVISIONS),
                    )),
                ),
            ],
        ),
    ]
}

/// The Style category: the fill, then the stroke, new objects are drawn in.
fn style(record: &Preferences) -> Vec<FieldGroup> {
    let mut stroke: Vec<FieldRow> = paint_rows("Stroke", record.stroke).into();
    stroke.push(
        length_row("Width", record.stroke_width, (0, MOST_LENGTH))
            .with_description("In drawing pixels."),
    );
    vec![
        FieldGroup::new("FILL", paint_rows("Fill", record.fill).into()),
        FieldGroup::new("STROKE", stroke).with_footnote(
            "New windows start in this style; a window's Style tab changes its own.",
        ),
    ]
}

/// The Editing category: how far the keys move things, and how near the
/// pointer must come.
fn editing(record: &Preferences) -> Vec<FieldGroup> {
    vec![
        FieldGroup::new(
            "MOVING",
            vec![
                length_row("Nudge", record.nudge, (1, MOST_LENGTH))
                    .with_description("In drawing pixels; ten times as far with Shift."),
                length_row(
                    "Duplicate offset",
                    record.duplicate_offset,
                    (0, MOST_LENGTH),
                )
                .with_description("Across and down, in drawing pixels."),
            ],
        ),
        FieldGroup::new(
            "PICKING",
            vec![FieldRow::new(
                "Pick distance",
                FieldControl::Number(NumberField::new(
                    to_i32(record.pick),
                    to_i32(PICK_REACH.0),
                    to_i32(PICK_REACH.1),
                )),
            )
            .with_description("In screen pixels, to a handle, a node or a guide, and to snap.")],
        ),
    ]
}

/// The number a number row's action holds.
const fn number_of(action: &FieldAction) -> Option<i32> {
    match action {
        FieldAction::Number(number) => Some(number.value()),
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

/// `value` as a count, none where it is negative.
fn unsigned(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// Carry `action` of General row `row` into `record`.
fn edit_general(record: &mut Preferences, row: usize, action: &FieldAction) {
    match row {
        row::TOOL => {
            if let Some(tool) = chosen(action).and_then(|index| Tool::ALL.get(index)) {
                record.tool = *tool;
            }
        }
        row::GRID => record.show.grid = switched(action).unwrap_or(record.show.grid),
        row::SNAP => record.show.snapping = switched(action).unwrap_or(record.show.snapping),
        row::OUTLINE => record.show.outline = switched(action).unwrap_or(record.show.outline),
        _ => {}
    }
}

/// Carry `action` of the New drawing category's row `row` of group `group`
/// into `record`.
fn edit_new_drawing(record: &mut Preferences, (group, row): (usize, usize), action: &FieldAction) {
    match (group, row) {
        (0, row::PAPER) => {
            if let Some(index) = chosen(action).filter(|&index| index < PAPER.len()) {
                record.paper = index;
            }
        }
        (0, row::ORIENTATION) => {
            if let Some(index) = chosen(action) {
                record.landscape = index == 1;
            }
        }
        (0, row::UNIT) => {
            if let Some(unit) = chosen(action).and_then(|index| Unit::ALL.get(index)) {
                record.unit = *unit;
            }
        }
        (1, row::SPACING) => {
            if let Some(value) = number_of(action) {
                let in_unit = pixels(unsigned(value)) * record.unit.pixels();
                let spacing = u32::try_from(hundredths(in_unit)).unwrap_or(0);
                record.grid_spacing = spacing.clamp(1, MOST_GRID_SPACING);
            }
        }
        (1, row::SUBDIVISIONS) => {
            if let Some(value) = number_of(action) {
                record.subdivisions = unsigned(value).clamp(1, MAX_SUBDIVISIONS);
            }
        }
        _ => {}
    }
}

/// Carry `action` of the Style category's row `row` of group `group` into
/// `record`: choosing a colour where there was none starts from Draw's own.
fn edit_style(record: &mut Preferences, (group, row): (usize, usize), action: &FieldAction) {
    if let (false, row::WIDTH, Some(value)) = (group == FILL, row, number_of(action)) {
        record.stroke_width = unsigned(value).min(MOST_LENGTH);
        return;
    }
    let (paint, default) = if group == FILL {
        (&mut record.fill, Preferences::default().fill)
    } else {
        (&mut record.stroke, Preferences::default().stroke)
    };
    match (row, action) {
        (row::PAINTED, FieldAction::Selected { index }) => {
            *paint = match (*index, *paint) {
                (0, _) => None,
                (_, None) => default,
                (_, held) => held,
            };
        }
        (row::COLOUR, FieldAction::Colour(colour)) if paint.is_some() => {
            *paint = Some(colour.colour());
        }
        _ => {}
    }
}

/// Carry `action` of the Editing category's row `row` of group `group` into
/// `record`.
fn edit_editing(record: &mut Preferences, (group, row): (usize, usize), action: &FieldAction) {
    let Some(value) = number_of(action).map(unsigned) else {
        return;
    };
    match (group, row) {
        (0, row::NUDGE) => record.nudge = value.clamp(1, MOST_LENGTH),
        (0, row::DUPLICATE) => record.duplicate_offset = value.min(MOST_LENGTH),
        (1, row::PICK) => record.pick = value.clamp(PICK_REACH.0, PICK_REACH.1),
        _ => {}
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
