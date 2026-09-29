//! The vectorised cursor representation.
//!
//! A cursor is not a fixed-resolution bitmap mask: it is a small stack of
//! filled [`Shape`]s over a square design grid, plus a hotspot and an
//! optional [`Outline`], so the same definition rasterises crisply at any
//! scale ([`VectorCursor::rasterise`]) and carries real colour and alpha
//! rather than a single foreground bit.
//!
//! Shapes are painted in order, each composited *over* the ones below it
//! through `lib/raster`'s single premultiplied-alpha path. A built-in cursor
//! is a flat stack; a decoded SVG one may also carry groups, where a clip, a
//! mask, or a group opacity composites part of the artwork as a unit.

use alloc::vec::Vec;

use tairix_raster::{Color, Node};

/// One filled, colourful layer of a cursor: what it is painted with, which
/// points it encloses, and the contours that bound them, in design-grid
/// coordinates.
///
/// The shared artwork layer, so a built-in cursor and a document decoded by
/// `lib/svg` are the same thing to the rasteriser.
pub use tairix_raster::Layer as Shape;

/// A contrasting band beneath a cursor's silhouette, which is what keeps a
/// pointer legible over a background of its own colour.
///
/// Declared rather than drawn because a drawn band is stretched with the
/// artwork, so it lands on fractional pixels and comes out a different
/// weight on every edge. The rasteriser strokes the fitted silhouette with a
/// band a whole number of pixels wide instead, so the rim is equally thick
/// all the way round at every size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Outline {
    /// What the band is painted with.
    pub color: Color,
    /// How far the band reaches past the silhouette, in design units. It is
    /// drawn at the nearest whole number of pixels, and never under one.
    pub width: u32,
}

/// A complete cursor: a hotspot, filled artwork over a square design grid,
/// and optionally the [`Outline`] drawn around that artwork.
///
/// The design grid is `design_size` units on each side. The **hotspot** —
/// the design-grid point that tracks the pointer position — is held in the
/// same units, so it scales with the artwork. A cursor with no artwork is
/// legal (it rasterises to a fully transparent image); a degenerate
/// `design_size` of zero is not renderable and the rasteriser reports that by
/// returning `None` rather than panicking.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorCursor {
    design_size: u32,
    hotspot_x: i32,
    hotspot_y: i32,
    nodes: Vec<Node>,
    outline: Option<Outline>,
}

impl VectorCursor {
    /// Construct a cursor from its design-grid side, hotspot, and a flat
    /// shape stack (bottom shape first), which is what a built-in cursor is.
    #[must_use]
    pub fn new(design_size: u32, hotspot_x: i32, hotspot_y: i32, shapes: Vec<Shape>) -> Self {
        Self::from_artwork(
            design_size,
            hotspot_x,
            hotspot_y,
            shapes.into_iter().map(Node::Fill).collect(),
        )
    }

    /// Construct a cursor from artwork that may carry composited groups,
    /// which is what a decoded document is.
    #[must_use]
    pub const fn from_artwork(
        design_size: u32,
        hotspot_x: i32,
        hotspot_y: i32,
        nodes: Vec<Node>,
    ) -> Self {
        Self {
            design_size,
            hotspot_x,
            hotspot_y,
            nodes,
            outline: None,
        }
    }

    /// This cursor with `outline` drawn around its silhouette.
    #[must_use]
    pub fn with_outline(self, outline: Outline) -> Self {
        Self {
            outline: Some(outline),
            ..self
        }
    }

    /// The side length of the square design grid, in design units.
    #[must_use]
    pub const fn design_size(&self) -> u32 {
        self.design_size
    }

    /// The hotspot x-coordinate in design units.
    #[must_use]
    pub const fn hotspot_x(&self) -> i32 {
        self.hotspot_x
    }

    /// The hotspot y-coordinate in design units.
    #[must_use]
    pub const fn hotspot_y(&self) -> i32 {
        self.hotspot_y
    }

    /// The artwork, bottom first.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The band drawn around the artwork, if the cursor has one.
    #[must_use]
    pub const fn outline(&self) -> Option<Outline> {
        self.outline
    }
}
