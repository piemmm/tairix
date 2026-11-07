//! SVG, the drawing's native form (`plans/DRAW.md` DR5).
//!
//! [`read()`] reads a document through `lib/svg`'s own grammar and cascade —
//! its path commands, shapes, colours, lengths, transforms, stylesheets and
//! gradient definitions — into a drawing, keeping what SVG cannot say in
//! Draw's own namespace ([`DRAW_NAMESPACE`]): layers, live shapes and node
//! types, arrowheads, guides, the grid and the page's unit. What a drawing
//! cannot hold is recorded as [`crate::unkept::Unkept`]. [`write()`] writes a
//! drawing back as clean SVG 1.1 that other editors read, with Inkscape's
//! layer attributes beside Draw's own.

mod number;
mod read;
mod write;

pub use read::{read, Read, ReadError};
pub use write::write;

/// The namespace Draw's own attributes and elements live in.
pub const DRAW_NAMESPACE: &str = "urn:tairix:draw";

/// Inkscape's namespace, whose layer attributes Draw reads and writes so the
/// two editors see the same layers.
pub const INKSCAPE_NAMESPACE: &str = "http://www.inkscape.org/namespaces/inkscape";

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
