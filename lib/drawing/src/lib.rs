//! The editable vector drawing (`plans/DRAW.md` DR4–DR9).
//!
//! A [`Drawing`] is a page and its layers; each layer is a group, and a group
//! holds [`Node`]s — shapes, text and further groups, a group optionally
//! clipped by a node of its own. Every node is immutable once shared, behind
//! an `Arc`: an edit makes new nodes on the way to the root and shares
//! everything else, so a save or a render takes the drawing whole for the
//! price of a pointer, and an undo history holds only what left it.
//!
//! Every value that enters the tree is checked as it enters: a non-finite or
//! out-of-range coordinate, a gradient's stops out of order, a name too long,
//! a tree too deep or too large is refused with a [`DrawingError`], never
//! carried, so a drawing read from an untrusted file holds nothing a drawing
//! made by hand could not. Every shared node, style, text, list and string is
//! allocated fallibly (`tairix_collections`), so a heap that refuses one is a
//! refusal, never an abort.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::disallowed_methods)]

extern crate alloc;

pub mod children;
pub mod drawing;
pub mod eps;
pub mod hit;
pub mod id;
pub mod image;
pub mod limits;
pub mod node;
pub mod numbering;
pub mod pdf;
pub mod render;
pub mod riscos;
pub mod shape;
pub mod style;
pub mod svg;
pub mod text;
pub mod unkept;
mod vector;

pub use children::Children;
pub use drawing::{
    Drawing, Grid, Guide, GuideAxis, Info, NodePath, Page, Paper, Unit, PAPER, PAPER_WHITE,
    PIXELS_PER_INCH,
};
pub use id::{Ids, ObjectId};
pub use image::{decode_placed, pictures_fit, placeable, Encoding, Image, PlacedPicture, Source};
pub use node::{Kind, Node};
pub use shape::{ArcKind, ArcPart, Contour, NodeJoin, PathNode, PathShape, Shape};
pub use style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stop,
    Stroke, Style,
};
pub use text::{
    Baseline, Caret, Glyph, Glyphs, Painted, Pen, Placed, Run, RunPaint, RunStyle, Setting, Text,
    TextAlign, TextFace, TextPath, DEFAULT_FAMILY, DEFAULT_LINE_HEIGHT,
};

pub use tairix_collections::{SharedSlice, SharedStr};

use tairix_bezier::boolean::BooleanError;
use tairix_bezier::fit::FitError;
use tairix_collections::TryReserveError;

/// Read `bytes`, a drawing in any format the crate reads, told apart by its
/// content rather than its name: a RISC OS Draw file by its signature, any
/// other document as SVG. Its objects' identities are issued from `ids`.
///
/// # Errors
///
/// As the format's own reader.
pub fn read(bytes: &[u8], ids: &mut Ids) -> Result<Read, ReadError> {
    if riscos::recognised(bytes) {
        riscos::read(bytes, ids)
    } else {
        svg::read(bytes, ids)
    }
}

/// What the crate's tests share text, lists and glyphs with.
#[cfg(test)]
pub(crate) mod testing {
    use alloc::sync::Arc;

    use tairix_bezier::{Path, PathEl, Point, Subpath};
    use tairix_collections::{SharedSlice, SharedStr};

    use crate::text::{Baseline, Glyph, Glyphs, Text};

    /// No glyph is known.
    pub(crate) struct NoGlyphs;

    impl Glyphs for NoGlyphs {
        fn glyph(
            &self,
            _family: &str,
            _weight: u16,
            _italic: bool,
            _c: char,
        ) -> Option<Arc<Glyph>> {
            None
        }
    }

    /// Glyphs for `a` alone: a square filling its em.
    pub(crate) struct OnlyA;

    impl Glyphs for OnlyA {
        fn glyph(&self, _family: &str, _weight: u16, _italic: bool, c: char) -> Option<Arc<Glyph>> {
            (c == 'a').then(|| {
                Arc::new(Glyph::new(
                    ring(&[(0.0, -1.0), (1.0, -1.0), (1.0, 0.0), (0.0, 0.0)]),
                    1.0,
                ))
            })
        }
    }

    /// Every glyph an em wide and inkless but the narrow `i`, half one, and
    /// `?`, which no font has.
    pub(crate) struct Ems;

    impl Glyphs for Ems {
        fn glyph(&self, _family: &str, _weight: u16, _italic: bool, c: char) -> Option<Arc<Glyph>> {
            (c != '?').then(|| Arc::new(Glyph::new(Path::new(), if c == 'i' { 0.5 } else { 1.0 })))
        }
    }

    /// Glyphs claiming three ems each and inking past every side of their
    /// em, as a hostile font might.
    pub(crate) struct Wide;

    impl Glyphs for Wide {
        fn glyph(
            &self,
            _family: &str,
            _weight: u16,
            _italic: bool,
            _c: char,
        ) -> Option<Arc<Glyph>> {
            Some(Arc::new(Glyph::new(
                ring(&[(-0.9, -2.5), (3.4, -2.5), (3.4, 1.1), (-0.9, 1.1)]),
                3.0,
            )))
        }
    }

    /// Where `text`'s first baseline stands, set in lines.
    pub(crate) fn origin(text: &Text) -> Point {
        match &text.baseline {
            Baseline::Lines { origin, .. } => *origin,
            Baseline::Path(_) => panic!("a text set in lines"),
        }
    }

    /// `text`'s line height, set in lines.
    pub(crate) fn line_height(text: &Text) -> f64 {
        match &text.baseline {
            Baseline::Lines { line_height, .. } => *line_height,
            Baseline::Path(_) => panic!("a text set in lines"),
        }
    }

    /// The closed polygon through `corners`, as one path.
    fn ring(corners: &[(f64, f64)]) -> Path {
        let mut ring = Subpath::new(Point::new(corners[0].0, corners[0].1));
        ring.elements.extend(
            corners[1..]
                .iter()
                .map(|&(x, y)| PathEl::Line(Point::new(x, y))),
        );
        ring.closed = true;
        Path {
            subpaths: alloc::vec![ring],
        }
    }

    /// `text`, shared.
    pub(crate) fn shared(text: &str) -> SharedStr {
        SharedStr::try_from_str(text).expect("held")
    }

    /// `items`, shared.
    pub(crate) fn slice<T: Clone>(items: &[T]) -> SharedSlice<T> {
        SharedSlice::try_from_slice(items).expect("held")
    }

    /// A `width`×`height` picture whose pixel at `(x, y)` is `colour(x, y)`,
    /// straight RGBA.
    pub(crate) fn pixels(
        width: u32,
        height: u32,
        colour: impl Fn(u32, u32) -> [u8; 4],
    ) -> tairix_raster::Surface {
        let mut surface = tairix_raster::Surface::new(width, height).expect("a surface");
        for y in 0..height {
            for x in 0..width {
                let [r, g, b, a] = colour(x, y);
                surface.set(x, y, tairix_raster::Color::rgba(r, g, b, a).premultiply());
            }
        }
        surface
    }

    /// `pixels` as a PNG.
    pub(crate) fn png(pixels: &tairix_raster::Surface) -> alloc::vec::Vec<u8> {
        tairix_image::encode_png(&tairix_image::SurfacePicture::new(pixels)).expect("a png")
    }

    /// `pixels` as a baseline JPEG, transparency over white.
    pub(crate) fn jpeg(pixels: &tairix_raster::Surface) -> alloc::vec::Vec<u8> {
        let options = tairix_image::JpegOptions::new(95, [255, 255, 255]).expect("options");
        tairix_image::encode_jpeg(&tairix_image::SurfacePicture::new(pixels), options)
            .expect("a jpeg")
    }

    /// `bytes` as the source a reader keeps of them, with what they state.
    pub(crate) fn source_of(bytes: &[u8]) -> crate::image::Source {
        let encoding = match tairix_image::sniff(bytes) {
            Some(tairix_image::ImageFormat::Jpeg) => {
                crate::image::Encoding::Jpeg(tairix_image::jpeg_frame(bytes).expect("a frame"))
            }
            _ => crate::image::Encoding::Png,
        };
        crate::image::Source {
            encoding,
            bytes: slice(bytes),
        }
    }

    /// A picture node of `image`, laid by `transform`.
    pub(crate) fn image(
        ids: &mut crate::id::Ids,
        image: crate::image::Image,
        transform: tairix_bezier::Affine,
    ) -> alloc::sync::Arc<crate::node::Node> {
        let node = crate::node::Node::new(
            ids.issue(),
            crate::node::Kind::Image {
                image: alloc::sync::Arc::new(image),
            },
        )
        .expect("a picture");
        alloc::sync::Arc::new(node.with_transform(transform).expect("placed"))
    }
}

/// What reading a document came to: the drawing, and what it could not keep.
#[derive(Clone, Debug)]
pub struct Read {
    /// The drawing.
    pub drawing: Drawing,
    /// What of the document the drawing could not hold.
    pub unkept: unkept::Unkept,
}

/// Why a document could not be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReadError {
    /// It is not a document of the format it was read as.
    Unrecognised,
    /// It is a document of the format, but malformed.
    Malformed,
    /// It exceeds a containment bound.
    TooComplex,
    /// The memory to read it could not be had.
    OutOfMemory,
}

impl From<DrawingError> for ReadError {
    fn from(error: DrawingError) -> Self {
        match error {
            DrawingError::Invalid => Self::Malformed,
            DrawingError::TooComplex => Self::TooComplex,
            DrawingError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

/// Why a drawing, or a part of one, was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DrawingError {
    /// A value outside its range: a coordinate that is not finite or lies
    /// beyond [`limits::MAX_COORDINATE`], a negative width, a gradient's
    /// stops out of order, an empty name.
    Invalid,
    /// Past one of the fixed bounds in [`limits`].
    TooComplex,
    /// The memory the drawing needs could not be had.
    OutOfMemory,
}

impl From<BooleanError> for DrawingError {
    fn from(error: BooleanError) -> Self {
        match error {
            BooleanError::TooComplex => Self::TooComplex,
            BooleanError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

impl From<FitError> for DrawingError {
    fn from(error: FitError) -> Self {
        match error {
            FitError::TooComplex => Self::TooComplex,
            FitError::OutOfMemory => Self::OutOfMemory,
        }
    }
}

impl From<TryReserveError> for DrawingError {
    fn from(error: TryReserveError) -> Self {
        match error {
            TryReserveError::AllocFailed => Self::OutOfMemory,
            TryReserveError::CapacityOverflow => Self::TooComplex,
        }
    }
}

impl From<TryReserveError> for ReadError {
    fn from(error: TryReserveError) -> Self {
        DrawingError::from(error).into()
    }
}

/// The heap's refusal as `alloc` reports it, which the geometry passes on.
impl From<alloc::collections::TryReserveError> for DrawingError {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

impl From<alloc::collections::TryReserveError> for ReadError {
    fn from(error: alloc::collections::TryReserveError) -> Self {
        DrawingError::from(error).into()
    }
}
