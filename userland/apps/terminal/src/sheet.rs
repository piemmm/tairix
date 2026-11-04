//! The settings sheet's picture, retained between frames, and the rectangle
//! it owes its next paint.
//!
//! [`crate::render::Screen`] is this for the terminal's grid. The sheet's
//! controls report the rectangles they change, and retaining the picture is
//! what makes a report worth anything: the pixels outside it are already
//! right, so a slider drag repaints and presents only what moved.

use tairix_controls::damage;
use tairix_geometry::{Rect, Region, Scale};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::settings::Settings;

/// The sheet's painted picture and what it owes.
#[derive(Debug)]
pub struct SheetScreen {
    /// The picture as last painted, whole.
    surface: Surface,
    /// The rectangles the sheet's controls have reported since that paint.
    /// The shared sink, so a batch of pointer samples degrades to the box it
    /// may as well have been rather than growing a rectangle per sample.
    damage: Region,
    /// Whether the surface holds pixels no report describes, which is what
    /// makes the next paint cover the sheet: before the first paint, and after
    /// anything a control could not have reported — a re-theme, a scale
    /// change, a frame region the session took back.
    stale: bool,
}

impl SheetScreen {
    /// A `width_px` × `height_px` picture with nothing painted yet, so the
    /// first [`paint`](Self::paint) draws the whole sheet.
    ///
    /// Returns `None` only when those dimensions cannot be allocated, so the
    /// caller fails closed rather than panicking.
    #[must_use]
    pub fn new(width_px: u32, height_px: u32) -> Option<Self> {
        Some(Self {
            surface: Surface::new(width_px, height_px)?,
            damage: damage::sink(),
            stale: true,
        })
    }

    /// The painted picture.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Where the sheet's controls report what they change.
    ///
    /// Handed to the routing for one event or one drained batch; the paint
    /// then covers everything reported since it last ran.
    pub const fn sink(&mut self) -> &mut Region {
        &mut self.damage
    }

    /// Mark every pixel stale, so the next paint covers the sheet.
    ///
    /// For a change no control could have reported: a re-theme, a new scale,
    /// or a frame region that was released and re-attached and so holds none of
    /// the pixels a partial present would leave standing.
    pub const fn invalidate(&mut self) {
        self.stale = true;
    }

    /// Paint whatever is owed and answer the rectangle it covers, which is
    /// [`Rect::EMPTY`] when nothing was owed.
    ///
    /// `viewport` is the sheet's whole extent throughout: the sheet lays
    /// itself out against it and draws in its own coordinates, and the clip is
    /// what confines the result to the rectangle that moved — so a scoped
    /// paint is pixel-identical to the same rectangle of a whole one.
    pub fn paint(&mut self, sheet: &Settings, viewport: Rect, scale: Scale, theme: &Theme) -> Rect {
        let bounds = Rect::new(
            0,
            0,
            self.surface.width().min(viewport.width),
            self.surface.height().min(viewport.height),
        );
        let rect = if self.stale {
            bounds
        } else {
            self.damage.bounds().intersection(&bounds)
        };
        self.damage.clear();
        self.stale = false;
        if rect.is_empty() {
            return Rect::EMPTY;
        }
        let Some((x, y)) = rect.surface_origin() else {
            return Rect::EMPTY;
        };
        self.surface.with_clip(x, y, rect.width, rect.height, |s| {
            sheet.render(s, viewport, scale, theme);
        });
        rect
    }
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
mod tests;
