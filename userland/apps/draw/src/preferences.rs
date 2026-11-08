//! Draw's own settings: what a new window and a new drawing start with and
//! how the pointer and the keys move things, kept in Draw's private app-data
//! store by the shared closed-registry engine and edited in its settings
//! window.
//!
//! Lengths are kept in hundredths of a drawing pixel, as a settings store
//! spells no fraction.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, as_u32, bool_text, overwrite, Live, Registry};
use tairix_colour::{parse_hex, Rgba};
use tairix_drawing::limits::MAX_SUBDIVISIONS;
use tairix_drawing::{Grid, Page, Paint, Paper, Stroke, Style, Unit, PAPER};

use crate::tools::Tool;
use crate::view::Showing;

/// The widest a pick reaches, in logical pixels.
pub const PICK_REACH: (u32, u32) = (2, 32);

/// The longest a nudge, a duplicate's offset or a new stroke may be, in
/// hundredths of a drawing pixel: ten thousand pixels.
pub const MOST_LENGTH: u32 = 1_000_000;

/// The widest a new drawing's grid may be spaced, in hundredths of a drawing
/// pixel.
pub const MOST_GRID_SPACING: u32 = 10_000_000;

/// The paint new objects are given: none, or a colour.
pub type Colouring = Option<Rgba>;

/// Every setting Draw keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Preferences {
    /// The tool a new window starts with.
    pub tool: Tool,
    /// How a new window shows the drawing and helps place things on it.
    pub show: Showing,
    /// The paper a new drawing's page is, as `PAPER` lists it.
    pub paper: usize,
    /// Whether a new drawing's page stands wider than tall.
    pub landscape: bool,
    /// The unit a new drawing is measured in.
    pub unit: Unit,
    /// A new drawing's grid spacing, in hundredths of a drawing pixel.
    pub grid_spacing: u32,
    /// How many cells a new drawing's grid cuts each of its cells into.
    pub subdivisions: u32,
    /// The fill a new window draws new objects with.
    pub fill: Colouring,
    /// The stroke it draws them with.
    pub stroke: Colouring,
    /// That stroke's width, in hundredths of a drawing pixel.
    pub stroke_width: u32,
    /// How far an arrow key moves the selection, in hundredths of a drawing
    /// pixel; ten times as far with Shift.
    pub nudge: u32,
    /// How far a duplicate stands from what it copies, across and down, in
    /// hundredths of a drawing pixel.
    pub duplicate_offset: u32,
    /// How near, in logical pixels, the pointer must come to pick a handle, a
    /// node or a guide, and to snap.
    pub pick: u32,
}

impl Default for Preferences {
    fn default() -> Self {
        let grid = Grid::default();
        Self {
            tool: Tool::Select,
            show: Showing {
                outline: false,
                grid: false,
                snapping: true,
            },
            paper: 0,
            landscape: false,
            unit: PAPER[0].unit,
            grid_spacing: hundredths(grid.spacing),
            subdivisions: grid.subdivisions,
            fill: Some(Rgba::new(86, 142, 214, 255)),
            stroke: Some(Rgba::new(28, 32, 40, 255)),
            stroke_width: 100,
            nudge: 100,
            duplicate_offset: 1000,
            pick: 6,
        }
    }
}

impl Preferences {
    /// The paper a new drawing's page is.
    #[must_use]
    pub fn paper(&self) -> &'static Paper {
        PAPER.get(self.paper).unwrap_or(&PAPER[0])
    }

    /// The page a new drawing is laid out on.
    #[must_use]
    pub fn page(&self) -> Page {
        let held = Page::of(self.paper());
        let turned = if self.landscape {
            held.turned(true)
        } else {
            held
        };
        turned.measured_in(self.unit)
    }

    /// A new drawing's grid.
    #[must_use]
    pub fn grid(&self) -> Grid {
        Grid {
            spacing: pixels(self.grid_spacing),
            subdivisions: self.subdivisions,
        }
    }

    /// The style a new window draws new objects in: its fill and stroke, the
    /// rest of a style as it first is.
    #[must_use]
    pub fn style(&self) -> Style {
        let paint = |colouring: Colouring| colouring.map_or(Paint::None, Paint::Colour);
        Style {
            fill: paint(self.fill),
            stroke: Stroke {
                paint: paint(self.stroke),
                width: pixels(self.stroke_width),
                ..Stroke::default()
            },
            ..Style::default()
        }
    }

    /// How the pointer and the keys move things, as every window applies it.
    #[must_use]
    pub fn handling(&self) -> Handling {
        Handling {
            nudge: pixels(self.nudge),
            duplicate_offset: pixels(self.duplicate_offset),
            pick: self.pick,
        }
    }
}

/// How the pointer and the keys move things: what every open window takes up
/// as it settles.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Handling {
    /// How far an arrow key moves the selection, in drawing pixels.
    pub nudge: f64,
    /// How far a duplicate stands from what it copies, in drawing pixels.
    pub duplicate_offset: f64,
    /// How near the pointer must come to pick, in logical pixels.
    pub pick: u32,
}

impl Default for Handling {
    fn default() -> Self {
        Preferences::default().handling()
    }
}

/// `hundredths` of a drawing pixel, in pixels.
#[must_use]
pub fn pixels(hundredths: u32) -> f64 {
    f64::from(hundredths) / 100.0
}

/// `pixels`, a length, in whole hundredths.
fn hundredths(pixels: f64) -> u32 {
    u32::try_from(crate::inspector::hundredths(pixels)).unwrap_or(0)
}

/// One key Draw keeps a setting under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrefKey {
    /// `general.tool`.
    Tool,
    /// `view.grid`.
    Grid,
    /// `view.snap`.
    Snap,
    /// `view.outline`.
    Outline,
    /// `new.paper`.
    Paper,
    /// `new.landscape`.
    Landscape,
    /// `new.unit`.
    Unit,
    /// `new.grid-spacing`.
    GridSpacing,
    /// `new.grid-subdivisions`.
    Subdivisions,
    /// `style.fill`.
    Fill,
    /// `style.stroke`.
    Stroke,
    /// `style.stroke-width`.
    StrokeWidth,
    /// `edit.nudge`.
    Nudge,
    /// `edit.duplicate-offset`.
    DuplicateOffset,
    /// `edit.pick-distance`.
    Pick,
}

impl Registry for Preferences {
    type Key = PrefKey;
    const KEYS: &'static [PrefKey] = &[
        PrefKey::Tool,
        PrefKey::Grid,
        PrefKey::Snap,
        PrefKey::Outline,
        PrefKey::Paper,
        PrefKey::Landscape,
        PrefKey::Unit,
        PrefKey::GridSpacing,
        PrefKey::Subdivisions,
        PrefKey::Fill,
        PrefKey::Stroke,
        PrefKey::StrokeWidth,
        PrefKey::Nudge,
        PrefKey::DuplicateOffset,
        PrefKey::Pick,
    ];

    fn name(key: PrefKey) -> &'static str {
        match key {
            PrefKey::Tool => "general.tool",
            PrefKey::Grid => "view.grid",
            PrefKey::Snap => "view.snap",
            PrefKey::Outline => "view.outline",
            PrefKey::Paper => "new.paper",
            PrefKey::Landscape => "new.landscape",
            PrefKey::Unit => "new.unit",
            PrefKey::GridSpacing => "new.grid-spacing",
            PrefKey::Subdivisions => "new.grid-subdivisions",
            PrefKey::Fill => "style.fill",
            PrefKey::Stroke => "style.stroke",
            PrefKey::StrokeWidth => "style.stroke-width",
            PrefKey::Nudge => "edit.nudge",
            PrefKey::DuplicateOffset => "edit.duplicate-offset",
            PrefKey::Pick => "edit.pick-distance",
        }
    }

    fn read(&mut self, key: PrefKey, text: &str) -> bool {
        let number =
            |least: u32, most: u32| as_u32(text).ok().filter(|n| (least..=most).contains(n));
        let set = |slot: &mut u32, read: Option<u32>| read.map(|value| *slot = value).is_some();
        let flag = |slot: &mut bool| as_bool(text).map(|on| *slot = on).is_ok();
        match key {
            PrefKey::Tool => Tool::ALL
                .into_iter()
                .find(|tool| tool.name().eq_ignore_ascii_case(text))
                .map(|tool| self.tool = tool)
                .is_some(),
            PrefKey::Grid => flag(&mut self.show.grid),
            PrefKey::Snap => flag(&mut self.show.snapping),
            PrefKey::Outline => flag(&mut self.show.outline),
            PrefKey::Paper => PAPER
                .iter()
                .position(|paper| token_of(paper.name).eq_ignore_ascii_case(text))
                .map(|at| self.paper = at)
                .is_some(),
            PrefKey::Landscape => flag(&mut self.landscape),
            PrefKey::Unit => Unit::from_symbol(text)
                .map(|unit| self.unit = unit)
                .is_some(),
            PrefKey::GridSpacing => set(&mut self.grid_spacing, number(1, MOST_GRID_SPACING)),
            PrefKey::Subdivisions => set(&mut self.subdivisions, number(1, MAX_SUBDIVISIONS)),
            PrefKey::Fill => colouring(text).map(|fill| self.fill = fill).is_some(),
            PrefKey::Stroke => colouring(text).map(|stroke| self.stroke = stroke).is_some(),
            PrefKey::StrokeWidth => set(&mut self.stroke_width, number(0, MOST_LENGTH)),
            PrefKey::Nudge => set(&mut self.nudge, number(1, MOST_LENGTH)),
            PrefKey::DuplicateOffset => set(&mut self.duplicate_offset, number(0, MOST_LENGTH)),
            PrefKey::Pick => set(&mut self.pick, number(PICK_REACH.0, PICK_REACH.1)),
        }
    }

    fn spell(&self, key: PrefKey, out: &mut String) -> bool {
        let written = match key {
            PrefKey::Tool => {
                out.extend(self.tool.name().chars().map(|c| c.to_ascii_lowercase()));
                Ok(())
            }
            PrefKey::Grid => out.write_str(bool_text(self.show.grid)),
            PrefKey::Snap => out.write_str(bool_text(self.show.snapping)),
            PrefKey::Outline => out.write_str(bool_text(self.show.outline)),
            PrefKey::Paper => out.write_str(&token_of(self.paper().name)),
            PrefKey::Landscape => out.write_str(bool_text(self.landscape)),
            PrefKey::Unit => out.write_str(self.unit.symbol()),
            PrefKey::GridSpacing => write!(out, "{}", self.grid_spacing),
            PrefKey::Subdivisions => write!(out, "{}", self.subdivisions),
            PrefKey::Fill => spell_colouring(self.fill, out),
            PrefKey::Stroke => spell_colouring(self.stroke, out),
            PrefKey::StrokeWidth => write!(out, "{}", self.stroke_width),
            PrefKey::Nudge => write!(out, "{}", self.nudge),
            PrefKey::DuplicateOffset => write!(out, "{}", self.duplicate_offset),
            PrefKey::Pick => write!(out, "{}", self.pick),
        };
        written.is_ok()
    }
}

impl Live for Preferences {
    fn take(&mut self, other: &Self, key: PrefKey) -> bool {
        match key {
            PrefKey::Tool => overwrite(&mut self.tool, other.tool),
            PrefKey::Grid => overwrite(&mut self.show.grid, other.show.grid),
            PrefKey::Snap => overwrite(&mut self.show.snapping, other.show.snapping),
            PrefKey::Outline => overwrite(&mut self.show.outline, other.show.outline),
            PrefKey::Paper => overwrite(&mut self.paper, other.paper),
            PrefKey::Landscape => overwrite(&mut self.landscape, other.landscape),
            PrefKey::Unit => overwrite(&mut self.unit, other.unit),
            PrefKey::GridSpacing => overwrite(&mut self.grid_spacing, other.grid_spacing),
            PrefKey::Subdivisions => overwrite(&mut self.subdivisions, other.subdivisions),
            PrefKey::Fill => overwrite(&mut self.fill, other.fill),
            PrefKey::Stroke => overwrite(&mut self.stroke, other.stroke),
            PrefKey::StrokeWidth => overwrite(&mut self.stroke_width, other.stroke_width),
            PrefKey::Nudge => overwrite(&mut self.nudge, other.nudge),
            PrefKey::DuplicateOffset => {
                overwrite(&mut self.duplicate_offset, other.duplicate_offset)
            }
            PrefKey::Pick => overwrite(&mut self.pick, other.pick),
        }
    }
}

/// A paper's spelling in the store: its name lowercased, a run of anything
/// but letters and digits one hyphen — `us-letter`, `screen-1920-x-1080`.
#[must_use]
pub fn token_of(name: &str) -> String {
    let mut token = String::new();
    for character in name.chars() {
        let mapped = match character {
            '×' => Some('x'),
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match mapped {
            Some(c) => token.push(c),
            None if !token.ends_with('-') && !token.is_empty() => token.push('-'),
            None => {}
        }
    }
    if token.ends_with('-') {
        token.pop();
    }
    token
}

/// The paint `text` spells: `none`, or a colour's hex digits — six, or
/// eight with its opacity — without the `#` a settings store reads as a
/// comment.
fn colouring(text: &str) -> Option<Colouring> {
    if text == "none" {
        return Some(None);
    }
    parse_hex(text).map(|(colour, _)| Some(colour))
}

fn spell_colouring(colouring: Colouring, out: &mut String) -> core::fmt::Result {
    match colouring {
        None => out.write_str("none"),
        Some(colour) => write!(out, "{}", colour.hex()),
    }
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
