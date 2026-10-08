//! A rasterised cursor, and what may be done to one once drawn.
//!
//! [`CursorImage`] is pixels plus the hotspot in them. Beyond reading it, a
//! pointer may cast a soft shadow ([`CursorImage::shadowed`]) and be shown at
//! a size other than the one it was drawn at ([`CursorImage::resampled_to`]),
//! each through `lib/raster`'s one blur and one resampler, and each keeping the
//! hotspot where the artwork puts it.

use tairix_geometry::Point;
use tairix_raster::{cast_shadow, Color, Region, ResampleScratch, ShadowCast, Surface};
use tairix_reclaim::CachedBytes;

use crate::store::CURSOR_BASE_SIDE_PX;

/// How far right of the pointer its shadow falls, in pixels of a pointer
/// drawn at [`CURSOR_BASE_SIDE_PX`].
const SHADOW_DROP_X: u32 = 1;

/// How far below the pointer its shadow falls, at the same reference side:
/// further than it falls right, as light from above and to the left casts it.
const SHADOW_DROP_Y: u32 = 2;

/// The radius of each blur pass that softens the shadow, at the reference
/// side.
const SHADOW_SOFTNESS: u32 = 1;

/// The shadow where it is densest.
const SHADOW_INK: Color = Color::rgba(0, 0, 0, 100);

/// A rasterised cursor: an opaque-where-drawn pixel image plus the hotspot
/// expressed in that image's own pixel coordinates.
///
/// The window manager blits [`surface`](Self::surface) so that
/// [`hotspot`](Self::hotspot) lands on the pointer position; the surface is
/// transparent everywhere the cursor does not draw, so it composites over
/// the desktop correctly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    surface: Surface,
    hotspot: Point,
}

impl CachedBytes for CursorImage {
    /// The image's only heap allocation is its `Surface`'s pixel buffer;
    /// the hotspot is a plain `Copy` coordinate pair with no heap part.
    fn payload_bytes(&self) -> usize {
        self.surface.payload_bytes()
    }

    /// Delegate to the surface's own wipe: the hotspot carries no
    /// rendered data worth clearing.
    fn wipe(&mut self) {
        self.surface.wipe();
    }
}

impl CursorImage {
    /// An image of `surface` whose hotspot is `hotspot`.
    pub(crate) const fn new(surface: Surface, hotspot: Point) -> Self {
        Self { surface, hotspot }
    }

    /// The rendered pixels, transparent outside the cursor artwork.
    #[must_use]
    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    /// The hotspot in this image's pixel coordinates.
    #[must_use]
    pub const fn hotspot(&self) -> Point {
        self.hotspot
    }

    /// The image width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.surface.width()
    }

    /// The image height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.surface.height()
    }

    /// This image over the soft shadow it casts, or `None` when the memory
    /// for it cannot be had.
    ///
    /// The shadow is the artwork's own coverage, dropped below and to the
    /// right and softened, in proportion to the image's side so a larger
    /// pointer casts a proportionally larger shadow. The image grows to hold
    /// it; the hotspot moves with the artwork, so the pointer still lands
    /// exactly where it did.
    #[must_use]
    pub fn shadowed(&self) -> Option<Self> {
        let side = self.width().max(self.height());
        let at_side = |reference: u32| {
            let scaled = u64::from(reference) * u64::from(side);
            let rounded =
                (scaled + u64::from(CURSOR_BASE_SIDE_PX / 2)) / u64::from(CURSOR_BASE_SIDE_PX);
            u32::try_from(rounded).unwrap_or(u32::MAX).max(1)
        };
        let cast = ShadowCast {
            drop_x: at_side(SHADOW_DROP_X),
            drop_y: at_side(SHADOW_DROP_Y),
            radius: at_side(SHADOW_SOFTNESS),
        };
        let (mask, (left, top)) = cast_shadow(&self.surface, cast)?;
        let mut surface = Surface::new(mask.width(), mask.height())?;
        surface.blit_tinted(0, 0, &mask, SHADOW_INK);
        let (x, y) = (left.checked_neg()?, top.checked_neg()?);
        surface.blit(x, y, &self.surface);
        Some(Self {
            surface,
            hotspot: Point::new(
                self.hotspot.x.saturating_add(x),
                self.hotspot.y.saturating_add(y),
            ),
        })
    }

    /// This image resampled so its larger side is `side` pixels, or `None`
    /// for a zero side or memory that cannot be had.
    ///
    /// Drawn into `recycled`'s pixel buffer when one is given and filtered in
    /// `scratch`, so a pointer shown at a new size every frame allocates only
    /// while it outgrows the buffers it cycles through. The hotspot scales
    /// with the artwork, to the nearest pixel.
    #[must_use]
    pub fn resampled_to(
        &self,
        side: u32,
        recycled: Option<Self>,
        scratch: &mut ResampleScratch,
    ) -> Option<Self> {
        let own = self.width().max(self.height());
        if side == 0 || own == 0 {
            return None;
        }
        let scaled = |length: u32| {
            let length =
                (u64::from(length) * u64::from(side) + u64::from(own / 2)) / u64::from(own);
            u32::try_from(length).unwrap_or(u32::MAX)
        };
        let scaled_signed = |at: i32| {
            let at = (i64::from(at) * i64::from(side) + i64::from(own / 2)) / i64::from(own);
            i32::try_from(at).unwrap_or(i32::MAX)
        };
        let (width, height) = (scaled(self.width()).max(1), scaled(self.height()).max(1));
        let mut surface = match recycled {
            Some(image) => image.surface.reshaped(width, height)?,
            None => Surface::new(width, height)?,
        };
        let whole = Region {
            x: 0,
            y: 0,
            width: self.width(),
            height: self.height(),
        };
        self.surface
            .resample_into(whole, &mut surface, scratch)
            .ok()?;
        Some(Self {
            surface,
            hotspot: Point::new(scaled_signed(self.hotspot.x), scaled_signed(self.hotspot.y)),
        })
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
