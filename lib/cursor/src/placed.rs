//! Placing a rasterised cursor on screen and sampling it.
//!
//! [`CursorImage`] is artwork; where it goes is [`PlacedCursor`]. Every
//! surface that draws a pointer — the compositor's top-most overlay, the
//! login screen's own frame — places it the same way, so the hotspot lands
//! on the pointer in exactly one definition rather than one per screen.

use tairix_geometry::{Point, Rect};
use tairix_raster::color::Pixel;

use crate::image::CursorImage;

/// A rasterised cursor positioned on screen.
///
/// The stored origin is the image's top-left corner, derived from the
/// pointer position by subtracting the image's hotspot, so the hotspot
/// lands exactly on the pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedCursor {
    image: CursorImage,
    origin: Point,
}

impl PlacedCursor {
    /// Place `image` so its hotspot sits at `pointer`.
    #[must_use]
    pub fn new(image: CursorImage, pointer: Point) -> Self {
        let origin = top_left(&image, pointer);
        Self { image, origin }
    }

    /// Move the cursor so its hotspot sits at `pointer`.
    pub fn set_pointer(&mut self, pointer: Point) {
        self.origin = top_left(&self.image, pointer);
    }

    /// The image this cursor draws.
    #[must_use]
    pub const fn image(&self) -> &CursorImage {
        &self.image
    }

    /// The image, taken back from the screen it was placed on.
    #[must_use]
    pub fn into_image(self) -> CursorImage {
        self.image
    }

    /// The screen rectangle the cursor currently covers.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.covering(self.origin)
    }

    /// The screen rectangle this cursor's image would cover with its hotspot
    /// at `pointer` — where a copy of it drawn somewhere the pointer has been
    /// lands.
    #[must_use]
    pub fn bounds_at(&self, pointer: Point) -> Rect {
        self.covering(top_left(&self.image, pointer))
    }

    fn covering(&self, origin: Point) -> Rect {
        Rect::new(origin.x, origin.y, self.image.width(), self.image.height())
    }

    /// This cursor's local row for screen row `y`, or `None` when the row
    /// falls outside its image.
    ///
    /// A draw loop calls this once per dirty screen row rather than
    /// re-deriving the image-local `y` for every column in it.
    #[must_use]
    pub fn local_row(&self, y: i32) -> Option<u32> {
        let ly = u32::try_from(y.checked_sub(self.origin.y)?).ok()?;
        if ly >= self.image.height() {
            return None;
        }
        Some(ly)
    }

    /// The premultiplied cursor pixel at column `x` of local row `ly` (as
    /// produced by [`Self::local_row`]), or `None` where the cursor draws
    /// nothing there (outside its image, or a transparent pixel within it).
    ///
    /// The row is already resolved to an image-local `y`, so a draw loop pays
    /// that conversion once per row instead of once per pixel.
    #[must_use]
    pub fn sample_row(&self, x: i32, ly: u32) -> Option<Pixel> {
        let lx = u32::try_from(x.checked_sub(self.origin.x)?).ok()?;
        self.sample_local(lx, ly)
    }

    /// The premultiplied cursor pixel at *image-local* `(lx, ly)`, or
    /// `None` outside the image or where it draws nothing.
    fn sample_local(&self, lx: u32, ly: u32) -> Option<Pixel> {
        let pixel = self.image.surface().get(lx, ly)?;
        (pixel.a > 0).then_some(pixel)
    }
}

/// The image's top-left corner for a hotspot placed at `pointer`.
fn top_left(image: &CursorImage, pointer: Point) -> Point {
    let hotspot = image.hotspot();
    Point::new(
        pointer.x.saturating_sub(hotspot.x),
        pointer.y.saturating_sub(hotspot.y),
    )
}

#[cfg(test)]
#[path = "placed_tests.rs"]
mod tests;
