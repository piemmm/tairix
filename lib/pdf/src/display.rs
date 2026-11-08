//! A page's display list: what interpreting its content paints, in order.
//!
//! Every item holds its geometry in the space it was painted in and the
//! transform from there into the page's own space — points from the top left
//! of its rotated crop box, y down — so a stroke under a squashing transform
//! keeps the pen it was drawn with. Every colour is sRGB.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::offset::StrokeStyle;
use tairix_bezier::{Affine, FillRule, Path, Point};
use tairix_collections::SharedSlice;
use tairix_colour::Rgba;

/// What a fill or a stroke is painted with.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// One colour, its alpha the paint's opacity.
    Colour(Rgba),
    /// A gradient.
    Gradient(Arc<Gradient>),
}

/// A gradient's colour at one place along it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stop {
    /// Where, from 0 at its start to 1 at its end.
    pub offset: f64,
    /// Its colour, straight alpha.
    pub colour: Rgba,
}

/// Where a gradient runs, in its own space.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GradientKind {
    /// Along the line from `start` to `end`.
    Axial {
        /// Where offset 0 lies.
        start: Point,
        /// Where offset 1 lies.
        end: Point,
    },
    /// Between two circles, each offset the circle that blends them.
    Radial {
        /// The centre of the circle at offset 0.
        start: Point,
        /// Its radius.
        start_radius: f64,
        /// The centre of the circle at offset 1.
        end: Point,
        /// Its radius.
        end_radius: f64,
    },
}

/// A colour gradient: an axial or radial shading.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Where it runs.
    pub kind: GradientKind,
    /// Its colours, offsets ascending; two at one offset make a sharp step.
    pub stops: SharedSlice<Stop>,
    /// Whether it carries on past its start and past its end with the
    /// colours there; where not, nothing is painted past them.
    pub extend: (bool, bool),
    /// Its space into the space of what it paints.
    pub transform: Affine,
}

/// A stroke's pen.
#[derive(Clone, Debug, PartialEq)]
pub struct Pen {
    /// Its width, joins, miter limit and caps.
    pub style: StrokeStyle,
    /// Its dash pattern, alternately drawn and skipped; empty is solid.
    pub dashes: SharedSlice<f64>,
    /// How far into the pattern it begins.
    pub dash_offset: f64,
}

/// A raster picture: rows of straight-alpha sRGB pixels, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// Four bytes a pixel: red, green, blue and alpha.
    pub pixels: Vec<u8>,
    /// The JPEG it was decoded from, where those bytes alone are the picture,
    /// so it can be kept without decoding and encoding it again.
    pub jpeg: Option<SharedSlice<u8>>,
}

/// One thing a page paints.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// The inside of a path.
    Fill {
        /// The path.
        path: Path,
        /// Which points it encloses.
        rule: FillRule,
        /// What fills it.
        paint: Paint,
        /// Its space into the page's.
        transform: Affine,
    },
    /// A path's outline.
    Stroke {
        /// The path.
        path: Path,
        /// The pen it is drawn with.
        pen: Pen,
        /// What the outline is painted with.
        paint: Paint,
        /// Its space into the page's.
        transform: Affine,
    },
    /// What follows is clipped to the inside of a path, until the matching
    /// [`Item::Unclip`].
    Clip {
        /// The path.
        path: Path,
        /// Which points it encloses.
        rule: FillRule,
        /// Its space into the page's.
        transform: Affine,
    },
    /// The end of the innermost clip.
    Unclip,
    /// What follows, until the matching [`Item::Ungroup`], is drawn as one
    /// picture and laid down at an opacity.
    Group {
        /// The opacity it is laid down at.
        opacity: f64,
    },
    /// The end of the innermost group.
    Ungroup,
    /// A picture, filling the unit square its transform maps onto the page.
    Image {
        /// The picture.
        picture: Arc<Picture>,
        /// The unit square, top row at y = 0, into the page's space.
        transform: Affine,
        /// The opacity it is laid down at.
        opacity: f64,
    },
}

/// What a page holds that its display list does not.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lost {
    /// Soft masks other than a gradient's own alpha.
    SoftMasks,
    /// Blend modes other than normal.
    BlendModes,
    /// Tiling patterns.
    TilingPatterns,
    /// Shadings other than axial and radial.
    Shadings,
    /// Pictures that could not be read.
    Images,
    /// Text whose glyphs neither decode nor have outlines.
    Text,
    /// Annotations and form fields.
    Annotations,
    /// Content that was damaged, or past what a page may hold.
    Damaged,
}

impl Lost {
    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// Every part of a page its display list does not hold.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Losses(u16);

impl Losses {
    /// Record `lost`.
    pub fn insert(&mut self, lost: Lost) {
        self.0 |= lost.bit();
    }

    /// Whether `lost` was recorded.
    #[must_use]
    pub const fn contains(self, lost: Lost) -> bool {
        self.0 & lost.bit() != 0
    }

    /// Whether nothing was.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// What a page paints, bottom first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    /// Its width and height in the page's own space.
    pub size: (f64, f64),
    /// What it paints, in order.
    pub items: Vec<Item>,
    /// What the page holds that the items do not.
    pub lost: Losses,
}
