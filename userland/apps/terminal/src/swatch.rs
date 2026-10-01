//! The custom-scheme editor's twenty colour wells, laid over the shared
//! [`SwatchGrid`]: background, foreground, cursor, cursor text, then the
//! sixteen ANSI colours, five to a row.
//!
//! [`grid_for`] and [`apply`] are exact inverses: applying a grid built from
//! a scheme back onto a fresh copy of that scheme reproduces it exactly.

use alloc::vec::Vec;

use tairix_controls::SwatchGrid;
use tairix_raster::Color;

use crate::scheme::{ColorScheme, Rgb, ANSI_COLORS};

/// The wells a full scheme lays out: the four screen roles plus the sixteen
/// ANSI colours.
pub const WELL_COUNT: usize = ANSI_COLORS + 4;

/// Wells per row.
pub const COLUMNS: usize = 5;

/// The grid over `scheme`'s twenty colours, with the background selected.
#[must_use]
pub fn grid_for(scheme: &ColorScheme) -> SwatchGrid {
    SwatchGrid::new(COLUMNS, wells(scheme))
}

/// Show `scheme`'s colours, keeping the selected well and any press in
/// progress: the colours can change under a user still editing one.
pub fn adopt(grid: &mut SwatchGrid, scheme: &ColorScheme) {
    grid.adopt_colours(COLUMNS, wells(scheme));
}

/// Write the grid's twenty colours back onto `scheme`, in the order
/// [`grid_for`] reads them.
pub fn apply(grid: &SwatchGrid, scheme: &mut ColorScheme) {
    let well = |index| grid.colour(index).map_or(Rgb::default(), rgb);
    scheme.background = well(0);
    scheme.foreground = well(1);
    scheme.cursor = well(2);
    scheme.cursor_text = well(3);
    for (index, slot) in scheme.ansi.iter_mut().enumerate() {
        *slot = well(WELL_COUNT - ANSI_COLORS + index);
    }
}

/// Well `index`'s colour as the scheme holds it.
#[must_use]
pub fn colour(grid: &SwatchGrid, index: usize) -> Option<Rgb> {
    grid.colour(index).map(rgb)
}

/// A well's colour as a scheme spells it: the wells are always opaque.
const fn rgb(colour: Color) -> Rgb {
    Rgb::new(colour.r, colour.g, colour.b)
}

/// The twenty wells of `scheme`, in the documented order.
fn wells(scheme: &ColorScheme) -> Vec<Color> {
    [
        scheme.background,
        scheme.foreground,
        scheme.cursor,
        scheme.cursor_text,
    ]
    .into_iter()
    .chain(scheme.ansi)
    .map(Rgb::opaque)
    .collect()
}

#[cfg(test)]
#[path = "swatch_tests.rs"]
mod tests;
