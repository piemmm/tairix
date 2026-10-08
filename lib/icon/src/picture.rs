//! A decoded picture, and the way a draw site is handed one.

use alloc::vec::Vec;

use tairix_geometry::{Point, Rect};
use tairix_raster::Surface;
use tairix_reclaim::CachedBytes;

use crate::glyph::IconKind;

/// One decode: a picture's pixels, and — for one drawn smaller than its square,
/// as a thumbnail of a wide photograph is — where the picture itself lies
/// within it, which is where it is framed.
#[derive(Clone, Debug)]
pub struct Artwork {
    surface: Surface,
    frame: Option<Rect>,
}

impl Artwork {
    /// A picture that is the whole of `surface`, drawn unframed.
    #[must_use]
    pub const fn new(surface: Surface) -> Self {
        Self {
            surface,
            frame: None,
        }
    }

    /// A picture lying at `frame` within `surface`, framed where it is drawn.
    /// `None` for a frame that is empty or reaches outside the surface.
    #[must_use]
    pub fn framed(surface: Surface, frame: Rect) -> Option<Self> {
        let whole = Rect::new(0, 0, surface.width(), surface.height());
        (!frame.is_empty() && frame.intersection(&whole) == frame).then_some(Self {
            surface,
            frame: Some(frame),
        })
    }

    /// The pixels.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Where the picture lies within its pixels, when it is framed.
    #[must_use]
    pub const fn frame(&self) -> Option<Rect> {
        self.frame
    }

    /// The pixels, owned.
    #[must_use]
    pub fn into_surface(self) -> Surface {
        self.surface
    }
}

impl CachedBytes for Artwork {
    fn payload_bytes(&self) -> usize {
        self.surface.payload_bytes()
    }

    fn wipe(&mut self) {
        self.surface.wipe();
    }
}

/// A decode the rasteriser fitted inside its square: the square's
/// straight-alpha RGBA8 pixels, and where in it the picture lies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fitted {
    /// The square's pixels, row by row.
    pub pixels: Vec<u8>,
    /// Where the picture lies within the square.
    pub bounds: Rect,
}

/// The soft shadow a picture casts, as a draw site is handed it: a mask to
/// composite tinted, and where its origin lies from the picture's.
#[derive(Copy, Clone, Debug)]
pub struct CastShadow<'a> {
    /// The shadow's strength, in the mask's alpha.
    pub mask: &'a Surface,
    /// Where the mask's origin lies from the picture's origin.
    pub offset: Point,
}

/// The picture a draw site was handed for an icon.
///
/// Shipped artwork carries its own colours, while a glyph mask carries only
/// coverage and takes the colour the drawing control chooses for its state, so
/// which one it is travels with it rather than being inferred. A picture may
/// bring its frame (a thumbnail's own bounds) and, when the draw site asked for
/// one, the shadow it casts.
#[derive(Copy, Clone, Debug)]
pub struct IconPicture<'a> {
    surface: &'a Surface,
    mask: bool,
    frame: Option<Rect>,
    shadow: Option<CastShadow<'a>>,
    shadow_withheld: bool,
}

impl<'a> IconPicture<'a> {
    /// Ready-coloured artwork, composited as it is.
    #[must_use]
    pub const fn coloured(surface: &'a Surface) -> Self {
        Self {
            surface,
            mask: false,
            frame: None,
            shadow: None,
            shadow_withheld: false,
        }
    }

    /// A built-in glyph's coverage mask, composited tinted
    /// ([`Surface::blit_tinted`]).
    #[must_use]
    pub const fn mask(surface: &'a Surface) -> Self {
        Self {
            surface,
            mask: true,
            frame: None,
            shadow: None,
            shadow_withheld: false,
        }
    }

    /// `surface` as `kind`'s built-in picture: ready-coloured for a kind drawn
    /// as a badge, a mask to be tinted for any other.
    #[must_use]
    pub const fn builtin(kind: IconKind, surface: &'a Surface) -> Self {
        if kind.badge().is_some() {
            Self::coloured(surface)
        } else {
            Self::mask(surface)
        }
    }

    /// This picture, framed at `frame` within its pixels.
    #[must_use]
    pub const fn with_frame(mut self, frame: Option<Rect>) -> Self {
        self.frame = frame;
        self
    }

    /// This picture casting `shadow`.
    #[must_use]
    pub const fn with_shadow(mut self, shadow: CastShadow<'a>) -> Self {
        self.shadow = Some(shadow);
        self
    }

    /// The pixels.
    #[must_use]
    pub const fn surface(self) -> &'a Surface {
        self.surface
    }

    /// Whether the pixels are a coverage mask the drawing control tints.
    #[must_use]
    pub const fn is_mask(self) -> bool {
        self.mask
    }

    /// Where the picture lies within its pixels, when it is framed.
    #[must_use]
    pub const fn frame(self) -> Option<Rect> {
        self.frame
    }

    /// The shadow the picture casts, when the draw site asked for it.
    #[must_use]
    pub const fn shadow(self) -> Option<CastShadow<'a>> {
        self.shadow
    }

    /// This picture, its shadow asked for and not to be had — memory was
    /// short — so a draw site draws none rather than cast it again itself.
    #[must_use]
    pub const fn withholding_shadow(mut self) -> Self {
        self.shadow = None;
        self.shadow_withheld = true;
        self
    }

    /// Whether the shadow was asked for and withheld.
    #[must_use]
    pub const fn shadow_withheld(self) -> bool {
        self.shadow_withheld
    }

    /// The shipped artwork this picture is, or `None` when it is a glyph mask.
    ///
    /// For a caller that *stores* a picture instead of drawing it now — a
    /// taskbar slot keeping its application's icon: a mask takes its colour
    /// from whichever control draws it, so it is not finished pixels and cannot
    /// be held as though it were.
    #[must_use]
    pub const fn artwork(self) -> Option<&'a Surface> {
        if self.mask {
            None
        } else {
            Some(self.surface)
        }
    }
}
