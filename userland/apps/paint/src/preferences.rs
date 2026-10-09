//! Paint's own settings: what a new window starts with and how a picture is
//! drawn in one, kept in Paint's private app-data store by the shared
//! closed-registry engine and edited in its settings window.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, as_u32, bool_text, overwrite, Live, Registry, PERMILLE_FULL};
use tairix_colour::Rgb;

use crate::canvas::MAX_SIDE;
use crate::document::{colours_for, NewPicture, COLOURS};
use crate::pane::Arrangement;
use crate::save::SaveFormat;
use crate::tool::Tool;

/// The widest a grid cell may be, in picture pixels.
pub const MOST_GRID_SPACING: u32 = 1024;

/// The faintest a grid may be drawn, in permille: one fainter would vanish.
pub const LEAST_GRID_OPACITY: u32 = 50;

/// The zooms, in percent, from which the pixel grid may be asked to show:
/// below double size there is no room between pixels to draw it.
pub const PIXEL_GRID_ZOOMS: (u32, u32) = (200, 6400);

/// The checkerboard's square, in logical pixels.
pub const CHECKER_SIDES: (u32, u32) = (2, 64);

/// What a picture first opens at.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum OpenAt {
    /// Fitted to the window, never past actual size.
    #[default]
    Fitted,
    /// At actual size.
    Actual,
}

impl OpenAt {
    /// Both, in the order a choice lists them.
    pub const ALL: [Self; 2] = [Self::Fitted, Self::Actual];

    /// What a choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fitted => "Fitted to the window",
            Self::Actual => "Actual size",
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Fitted => "fitted",
            Self::Actual => "actual",
        }
    }
}

/// How the grid is drawn.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum GridStyle {
    /// Unbroken lines.
    #[default]
    Lines,
    /// Lines broken into dashes.
    Dashes,
    /// A dot at every crossing and along every line.
    Dots,
    /// A small cross at every crossing.
    Crossings,
}

impl GridStyle {
    /// Every style, in the order a choice lists them.
    pub const ALL: [Self; 4] = [Self::Lines, Self::Dashes, Self::Dots, Self::Crossings];

    /// What a choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lines => "Lines",
            Self::Dashes => "Dashes",
            Self::Dots => "Dots",
            Self::Crossings => "Crossings",
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Lines => "lines",
            Self::Dashes => "dashes",
            Self::Dots => "dots",
            Self::Crossings => "crossings",
        }
    }
}

/// The grid laid over a picture, measured in its pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Grid {
    /// A cell's width and height.
    pub spacing: (u32, u32),
    /// Where the first line stands from the picture's top left, across and
    /// down.
    pub offset: (u32, u32),
    /// The colour it is drawn in.
    pub colour: Rgb,
    /// How opaque it is drawn, in permille.
    pub opacity: u32,
    /// How it is drawn.
    pub style: GridStyle,
    /// Whether a new window shows it.
    pub shown: bool,
    /// Whether what is drawn and marked lands on its crossings.
    pub snap: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            spacing: (16, 16),
            offset: (0, 0),
            colour: Rgb::new(0x4a, 0x8f, 0xe2),
            opacity: 500,
            style: GridStyle::Lines,
            shown: false,
            snap: false,
        }
    }
}

/// The checkerboard's two shades.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Shades {
    /// The theme's own.
    #[default]
    Theme,
    /// These two, darker first.
    Chosen(Rgb, Rgb),
}

/// What surrounds the picture in its window.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Surround {
    /// The theme's ground.
    #[default]
    Theme,
    /// This colour.
    Chosen(Rgb),
}

/// Every setting Paint keeps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preferences {
    /// The tool a new window starts with.
    pub tool: Tool,
    /// What a picture opens at.
    pub open_at: OpenAt,
    /// What *New picture* offers to start.
    pub new: NewPicture,
    /// The format *New picture* offers to start.
    pub format: SaveFormat,
    /// The grid.
    pub grid: Grid,
    /// The zoom, in percent, from which lines between pixels are drawn, or
    /// none at all at `0`.
    pub pixel_grid_from: u32,
    /// The checkerboard's square, in logical pixels.
    pub checker_side: u32,
    /// The checkerboard's shades.
    pub shades: Shades,
    /// What surrounds the picture.
    pub surround: Surround,
    /// The panes a new window opens with.
    pub panes: Arrangement,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            tool: Tool::Brush,
            open_at: OpenAt::Fitted,
            new: NewPicture::DEFAULT,
            format: SaveFormat::Png,
            grid: Grid::default(),
            pixel_grid_from: 800,
            checker_side: 8,
            shades: Shades::Theme,
            surround: Surround::Theme,
            panes: Arrangement::default(),
        }
    }
}

/// What a window draws a picture with: the settings that apply to every
/// window as they settle.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CanvasStyle {
    /// The grid.
    pub grid: Grid,
    /// The zoom, in percent, from which lines between pixels are drawn, or
    /// none at all at `0`.
    pub pixel_grid_from: u32,
    /// The checkerboard's square, in logical pixels.
    pub checker_side: u32,
    /// The checkerboard's shades.
    pub shades: Shades,
    /// What surrounds the picture.
    pub surround: Surround,
}

impl Default for CanvasStyle {
    fn default() -> Self {
        Preferences::default().canvas_style()
    }
}

impl Preferences {
    /// What a window draws with.
    #[must_use]
    pub const fn canvas_style(&self) -> CanvasStyle {
        CanvasStyle {
            grid: self.grid,
            pixel_grid_from: self.pixel_grid_from,
            checker_side: self.checker_side,
            shades: self.shades,
            surround: self.surround,
        }
    }
}

/// One key Paint keeps a setting under.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrefKey {
    /// `general.tool`.
    Tool,
    /// `general.open-at`.
    OpenAt,
    /// `new.width`.
    NewWidth,
    /// `new.height`.
    NewHeight,
    /// `new.format`: read before the colours and background it decides.
    NewFormat,
    /// `new.colours`.
    NewColours,
    /// `new.transparent`.
    NewTransparent,
    /// `grid.across`.
    GridAcross,
    /// `grid.down`.
    GridDown,
    /// `grid.offset-across`.
    GridOffsetAcross,
    /// `grid.offset-down`.
    GridOffsetDown,
    /// `grid.colour`.
    GridColour,
    /// `grid.opacity`.
    GridOpacity,
    /// `grid.style`.
    GridStyle,
    /// `grid.shown`.
    GridShown,
    /// `grid.snap`.
    GridSnap,
    /// `grid.pixels-from`.
    PixelGridFrom,
    /// `canvas.checker-size`.
    CheckerSide,
    /// `canvas.checker-shades`.
    CheckerShades,
    /// `canvas.surround`.
    Surround,
    /// `panes.layout`.
    Panes,
}

impl Registry for Preferences {
    type Key = PrefKey;
    const KEYS: &'static [PrefKey] = &[
        PrefKey::Tool,
        PrefKey::OpenAt,
        PrefKey::NewWidth,
        PrefKey::NewHeight,
        PrefKey::NewFormat,
        PrefKey::NewColours,
        PrefKey::NewTransparent,
        PrefKey::GridAcross,
        PrefKey::GridDown,
        PrefKey::GridOffsetAcross,
        PrefKey::GridOffsetDown,
        PrefKey::GridColour,
        PrefKey::GridOpacity,
        PrefKey::GridStyle,
        PrefKey::GridShown,
        PrefKey::GridSnap,
        PrefKey::PixelGridFrom,
        PrefKey::CheckerSide,
        PrefKey::CheckerShades,
        PrefKey::Surround,
        PrefKey::Panes,
    ];

    fn name(key: PrefKey) -> &'static str {
        match key {
            PrefKey::Tool => "general.tool",
            PrefKey::OpenAt => "general.open-at",
            PrefKey::NewWidth => "new.width",
            PrefKey::NewHeight => "new.height",
            PrefKey::NewFormat => "new.format",
            PrefKey::NewColours => "new.colours",
            PrefKey::NewTransparent => "new.transparent",
            PrefKey::GridAcross => "grid.across",
            PrefKey::GridDown => "grid.down",
            PrefKey::GridOffsetAcross => "grid.offset-across",
            PrefKey::GridOffsetDown => "grid.offset-down",
            PrefKey::GridColour => "grid.colour",
            PrefKey::GridOpacity => "grid.opacity",
            PrefKey::GridStyle => "grid.style",
            PrefKey::GridShown => "grid.shown",
            PrefKey::GridSnap => "grid.snap",
            PrefKey::PixelGridFrom => "grid.pixels-from",
            PrefKey::CheckerSide => "canvas.checker-size",
            PrefKey::CheckerShades => "canvas.checker-shades",
            PrefKey::Surround => "canvas.surround",
            PrefKey::Panes => "panes.layout",
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
            PrefKey::OpenAt => token_of(OpenAt::ALL, OpenAt::token, text)
                .map(|open_at| self.open_at = open_at)
                .is_some(),
            PrefKey::NewWidth => set(&mut self.new.size.0, number(1, MAX_SIDE)),
            PrefKey::NewHeight => set(&mut self.new.size.1, number(1, MAX_SIDE)),
            PrefKey::NewFormat => token_of(SaveFormat::ALL, format_token, text)
                .map(|format| self.format = format)
                .is_some(),
            PrefKey::NewColours => colours_for(self.format)
                .find(|colours| colours.token == text)
                .map(|colours| self.new.depth = colours.depth)
                .is_some(),
            PrefKey::NewTransparent => match as_bool(text) {
                Ok(clear) if !clear || self.format.holds_transparency() => {
                    self.new.transparent = clear;
                    true
                }
                _ => false,
            },
            PrefKey::GridAcross => set(&mut self.grid.spacing.0, number(1, MOST_GRID_SPACING)),
            PrefKey::GridDown => set(&mut self.grid.spacing.1, number(1, MOST_GRID_SPACING)),
            PrefKey::GridOffsetAcross => {
                set(&mut self.grid.offset.0, number(0, MOST_GRID_SPACING - 1))
            }
            PrefKey::GridOffsetDown => {
                set(&mut self.grid.offset.1, number(0, MOST_GRID_SPACING - 1))
            }
            PrefKey::GridColour => Rgb::from_hex(text)
                .map(|colour| self.grid.colour = colour)
                .is_some(),
            PrefKey::GridOpacity => set(
                &mut self.grid.opacity,
                number(LEAST_GRID_OPACITY, PERMILLE_FULL),
            ),
            PrefKey::GridStyle => token_of(GridStyle::ALL, GridStyle::token, text)
                .map(|style| self.grid.style = style)
                .is_some(),
            PrefKey::GridShown => flag(&mut self.grid.shown),
            PrefKey::GridSnap => flag(&mut self.grid.snap),
            PrefKey::PixelGridFrom => {
                let (least, most) = PIXEL_GRID_ZOOMS;
                let read = as_u32(text)
                    .ok()
                    .filter(|&zoom| zoom == 0 || (least..=most).contains(&zoom));
                set(&mut self.pixel_grid_from, read)
            }
            PrefKey::CheckerSide => set(
                &mut self.checker_side,
                number(CHECKER_SIDES.0, CHECKER_SIDES.1),
            ),
            PrefKey::CheckerShades => {
                let shades = if text == "theme" {
                    Some(Shades::Theme)
                } else {
                    let mut colours = text.split_whitespace().map(Rgb::from_hex);
                    match (colours.next(), colours.next(), colours.next()) {
                        (Some(Some(dark)), Some(Some(light)), None) => {
                            Some(Shades::Chosen(dark, light))
                        }
                        _ => None,
                    }
                };
                shades.map(|shades| self.shades = shades).is_some()
            }
            PrefKey::Surround => {
                let surround = if text == "theme" {
                    Some(Surround::Theme)
                } else {
                    Rgb::from_hex(text).map(Surround::Chosen)
                };
                surround.map(|surround| self.surround = surround).is_some()
            }
            PrefKey::Panes => Arrangement::parse(text)
                .map(|panes| self.panes = panes)
                .is_some(),
        }
    }

    fn spell(&self, key: PrefKey, out: &mut String) -> bool {
        let written = match key {
            PrefKey::Tool => {
                for letter in self.tool.name().chars() {
                    out.push(letter.to_ascii_lowercase());
                }
                Ok(())
            }
            PrefKey::OpenAt => out.write_str(self.open_at.token()),
            PrefKey::NewWidth => write!(out, "{}", self.new.size.0),
            PrefKey::NewHeight => write!(out, "{}", self.new.size.1),
            PrefKey::NewFormat => out.write_str(format_token(self.format)),
            PrefKey::NewColours => {
                let token = COLOURS
                    .into_iter()
                    .find(|colours| colours.depth == self.new.depth)
                    .map_or(COLOURS[0].token, |colours| colours.token);
                out.write_str(token)
            }
            PrefKey::NewTransparent => out.write_str(bool_text(self.new.transparent)),
            PrefKey::GridAcross => write!(out, "{}", self.grid.spacing.0),
            PrefKey::GridDown => write!(out, "{}", self.grid.spacing.1),
            PrefKey::GridOffsetAcross => write!(out, "{}", self.grid.offset.0),
            PrefKey::GridOffsetDown => write!(out, "{}", self.grid.offset.1),
            PrefKey::GridColour => write!(out, "{}", self.grid.colour.hex()),
            PrefKey::GridOpacity => write!(out, "{}", self.grid.opacity),
            PrefKey::GridStyle => out.write_str(self.grid.style.token()),
            PrefKey::GridShown => out.write_str(bool_text(self.grid.shown)),
            PrefKey::GridSnap => out.write_str(bool_text(self.grid.snap)),
            PrefKey::PixelGridFrom => write!(out, "{}", self.pixel_grid_from),
            PrefKey::CheckerSide => write!(out, "{}", self.checker_side),
            PrefKey::CheckerShades => match self.shades {
                Shades::Theme => out.write_str("theme"),
                Shades::Chosen(dark, light) => write!(out, "{} {}", dark.hex(), light.hex()),
            },
            PrefKey::Surround => match self.surround {
                Surround::Theme => out.write_str("theme"),
                Surround::Chosen(colour) => write!(out, "{}", colour.hex()),
            },
            PrefKey::Panes => {
                self.panes.spell(out);
                Ok(())
            }
        };
        written.is_ok()
    }

    /// A format read with colours or a background it cannot hold keeps the
    /// format and takes what it can.
    fn normalise(&mut self) {
        if !self.format.admits(self.new.depth) {
            self.new.depth = colours_for(self.format)
                .next()
                .and_then(|colours| colours.depth);
        }
        self.new.transparent &= self.format.holds_transparency();
    }
}

impl Live for Preferences {
    fn take(&mut self, other: &Self, key: PrefKey) -> bool {
        match key {
            PrefKey::Tool => overwrite(&mut self.tool, other.tool),
            PrefKey::OpenAt => overwrite(&mut self.open_at, other.open_at),
            PrefKey::NewWidth => overwrite(&mut self.new.size.0, other.new.size.0),
            PrefKey::NewHeight => overwrite(&mut self.new.size.1, other.new.size.1),
            PrefKey::NewFormat => overwrite(&mut self.format, other.format),
            PrefKey::NewColours => overwrite(&mut self.new.depth, other.new.depth),
            PrefKey::NewTransparent => overwrite(&mut self.new.transparent, other.new.transparent),
            PrefKey::GridAcross => overwrite(&mut self.grid.spacing.0, other.grid.spacing.0),
            PrefKey::GridDown => overwrite(&mut self.grid.spacing.1, other.grid.spacing.1),
            PrefKey::GridOffsetAcross => overwrite(&mut self.grid.offset.0, other.grid.offset.0),
            PrefKey::GridOffsetDown => overwrite(&mut self.grid.offset.1, other.grid.offset.1),
            PrefKey::GridColour => overwrite(&mut self.grid.colour, other.grid.colour),
            PrefKey::GridOpacity => overwrite(&mut self.grid.opacity, other.grid.opacity),
            PrefKey::GridStyle => overwrite(&mut self.grid.style, other.grid.style),
            PrefKey::GridShown => overwrite(&mut self.grid.shown, other.grid.shown),
            PrefKey::GridSnap => overwrite(&mut self.grid.snap, other.grid.snap),
            PrefKey::PixelGridFrom => overwrite(&mut self.pixel_grid_from, other.pixel_grid_from),
            PrefKey::CheckerSide => overwrite(&mut self.checker_side, other.checker_side),
            PrefKey::CheckerShades => overwrite(&mut self.shades, other.shades),
            PrefKey::Surround => overwrite(&mut self.surround, other.surround),
            PrefKey::Panes => {
                let changed = self.panes != other.panes;
                if changed {
                    self.panes.clone_from(&other.panes);
                }
                changed
            }
        }
    }
}

/// A format's spelling in the store.
const fn format_token(format: SaveFormat) -> &'static str {
    match format {
        SaveFormat::Png => "png",
        SaveFormat::Jpeg => "jpeg",
        SaveFormat::Gif => "gif",
        SaveFormat::Bmp => "bmp",
        SaveFormat::Tiff => "tiff",
        SaveFormat::Sprites => "sprite",
        SaveFormat::OpenRaster => "openraster",
    }
}

/// The one of `all` that `token` spells as `text`.
fn token_of<T: Copy, const N: usize>(
    all: [T; N],
    token: fn(T) -> &'static str,
    text: &str,
) -> Option<T> {
    all.into_iter().find(|&value| token(value) == text)
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
