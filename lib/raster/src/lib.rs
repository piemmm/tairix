//! Shared software rasterisation primitives (`lib/raster`).
//!
//! This crate is the single home of the desktop's premultiplied-alpha
//! colour arithmetic ([`color`]), its CPU pixel buffer ([`surface`]), and
//! the separable box [`blur`] every frosted surface shares — reached as
//! [`Surface::frost_region`], which frosts one rectangle in place for a
//! control's selected tile, and [`Surface::frost_from`], which frosts bands of
//! one from a backdrop split between the destination and a plane, for the
//! compositor's window backdrop; both run in a reusable [`BlurScratch`].
//! Both the compositing window manager (`userland/gui/wm`) and the
//! taskbar (`userland/gui/taskbar`) draw pixels, but neither may depend
//! on the other; the shared rasteriser therefore
//! lives in `lib/*`, exactly as `lib/geometry` owns the shared
//! coordinate types and `lib/theme` owns the shared design tokens.
//!
//! Vector artwork fills through one scan converter ([`scan`]): any number of
//! closed contours resolved under a [`FillRule`] and painted with a flat
//! colour, a gradient, or a repeated tile ([`paint`]), reached as
//! [`Surface::fill_contours`].
//! [`Affine`] is the transform that places such artwork, and the one a
//! gradient carries. A whole drawing — a cursor, an icon glyph, a decoded
//! SVG document — is an [`artwork`] tree of such layers, with the [`Group`]s
//! that clipping, masking and group opacity all need, drawn through the one
//! walk of it ([`Surface::draw_artwork`]).
//!
//! There is exactly one definition of the colour algebra here, so it is
//! never duplicated into a sibling crate. Compositing over an 8-bit
//! destination holds fewer levels than the picture beneath it had, so every
//! operator rounds at a caller-chosen bias ([`div255_biased`]): the plain
//! form rounds to nearest, and a translucent field — a wash here, a
//! translucent window in the compositor — varies it per pixel from a
//! [`DitherRow`] and stays smooth instead of contouring. A colour value
//! ([`Rgba`], or an opaque [`Rgb`]) meets that algebra at a single edge —
//! [`From<Rgba>`](Color) — so the conversion is owned in one place rather
//! than re-implemented by each consumer.
//!
//! [`Rgba`]: tairix_colour::Rgba
//! [`Rgb`]: tairix_colour::Rgb

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod affine;
pub mod artwork;
pub mod blur;
pub mod color;
pub mod dither;
pub mod paint;
pub mod reorient;
pub mod resample;
pub mod ring;
pub mod round;
pub mod scan;
pub mod shape;
pub mod surface;

#[cfg(test)]
mod tests;

pub use affine::Affine;
pub use artwork::{
    for_each_fill, layer_count, Group, Layer, Mask, MaskKind, Node, MAX_GROUP_DEPTH,
};
pub use blur::{
    box_blur, box_blur_coverage, soften_coverage, BlurScratch, Frosting, SOFTEN_PASSES,
};
pub use color::{
    blend_solid_span, blend_span, div255, div255_biased, mix_span, Color, Pixel, ROUND_NEAREST,
};
pub use dither::DitherRow;
pub use paint::{
    Gradient, GradientKind, GradientStop, Paint, Pattern, SpreadMethod, TileFold, MAX_TILE_EXTENT,
    MAX_TILE_FOLD,
};
pub use reorient::Reorient;
pub use resample::{resample, resample_window, Region, ResampleError, ResampleScratch, Rgba8Image};
pub use ring::{Ring, RingInk};
pub use round::{round_rect_coverage, round_rect_radius};
pub use scan::{Coverage, CoverageRows, FillRule, ScanScratch, MAX_DRAWING_EXTENT};
pub use shape::{Placed, Shape};
pub use surface::{band_rows, Canvas, RowBand, RowBands, Surface, MAX_SURFACE_PIXELS, SUBPIXEL};
