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
//! made by hand could not.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::disallowed_methods)]

extern crate alloc;

pub mod children;
pub mod drawing;
pub mod hit;
pub mod id;
pub mod limits;
pub mod node;
pub mod numbering;
pub mod pdf;
pub mod render;
pub mod shape;
pub mod style;
pub mod svg;
pub mod text;
pub mod unkept;

pub use children::Children;
pub use drawing::{Drawing, Grid, Guide, GuideAxis, Info, NodePath, Page, Paper, Unit, PAPER};
pub use id::{Ids, ObjectId};
pub use node::{Kind, Node};
pub use shape::{ArcKind, ArcPart, Contour, NodeJoin, PathNode, PathShape, Shape};
pub use style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stop,
    Stroke, Style,
};
pub use text::{Glyph, Glyphs, Placed, Setting, Text, TextAlign};

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
