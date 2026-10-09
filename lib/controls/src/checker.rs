//! The transparency checkerboard: what shows through where a picture is
//! clear, defined once for every surface that draws one.
//!
//! Its squares alternate between the theme's surface and that colour lifted
//! an eighth of the way to white — the same visible step in a dark theme and
//! a light one.

use tairix_geometry::Scale;
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

/// One square's side, in logical pixels.
const CHECKER_SIDE: u32 = 8;

/// How far the lighter square is lifted toward white, in eighths.
const LIFT_EIGHTHS: u32 = 1;

/// The checkerboard for a theme, at a size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Checker {
    dark: Color,
    light: Color,
    side: u32,
}

impl Checker {
    /// The checkerboard `theme` draws at `scale`.
    #[must_use]
    pub fn new(theme: &Theme, scale: Scale) -> Self {
        let dark = Color::from(theme.palette().surface);
        let lift = |channel: u8| {
            let lifted = u32::from(channel) + (255 - u32::from(channel)) * LIFT_EIGHTHS / 8;
            u8::try_from(lifted).unwrap_or(u8::MAX)
        };
        Self {
            dark,
            light: Color::rgba(lift(dark.r), lift(dark.g), lift(dark.b), dark.a),
            side: scale.scale_length(CHECKER_SIDE).max(1),
        }
    }

    /// This checkerboard with squares `side` pixels across.
    #[must_use]
    pub const fn with_side(mut self, side: u32) -> Self {
        self.side = if side == 0 { 1 } else { side };
        self
    }

    /// This checkerboard in `dark` and `light`, for a surface that lets its
    /// user choose them.
    #[must_use]
    pub const fn with_shades(mut self, dark: Color, light: Color) -> Self {
        self.dark = dark;
        self.light = light;
        self
    }

    /// A square's side, in pixels.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// The colour at `(x, y)` pixels from the board's top left: the dark
    /// square there, and the light one beside it.
    #[must_use]
    pub const fn at(&self, x: u32, y: u32) -> Color {
        if (x / self.side + y / self.side).is_multiple_of(2) {
            self.dark
        } else {
            self.light
        }
    }

    /// Paint the board over the `w`×`h` pixels from `(x, y)`, its squares
    /// counted from there.
    pub fn paint(&self, surface: &mut Surface, x: u32, y: u32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        surface.fill_rect(x, y, w, h, self.dark);
        let side = self.side;
        let mut row = 0;
        while row * side < h {
            let mut column = u32::from(row % 2 == 0);
            while column * side < w {
                surface.fill_rect(
                    x + column * side,
                    y + row * side,
                    side.min(w - column * side),
                    side.min(h - row * side),
                    self.light,
                );
                column += 2;
            }
            row += 1;
        }
    }
}

#[cfg(test)]
#[path = "checker_tests.rs"]
mod tests;
