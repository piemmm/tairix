//! Plane geometry for vector drawing (`plans/DRAW.md` DR1, DR2).
//!
//! One geometry currency below both rasterisation and SVG: [`Point`], the
//! [`Affine`] map, [`Bounds`], the [`FillRule`], the [`Line`], [`QuadBez`] and
//! [`CubicBez`] segments, the elliptical [`EllipseArc`], and the [`Path`] of
//! sub-paths built from them — and the algorithms over them: flattening,
//! winding and area, nearest points, arc length, intersections, curve fitting,
//! offsets and boolean operations.
//!
//! Every algorithm is total over finite input: no panic, no unbounded loop,
//! and no allocation that grows without a stated bound. Recursion depths,
//! intersections per pair and output segment counts are fixed containment
//! bounds, and work past one is refused as a typed error rather than done.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::disallowed_methods)]

extern crate alloc;

pub mod affine;
pub mod arc;
pub mod boolean;
pub mod bounds;
pub mod cubic;
pub mod fit;
pub mod flatten;
pub mod intersect;
pub mod line;
pub mod measure;
pub mod offset;
pub mod path;
pub mod point;
pub mod quad;
pub mod roots;
pub mod segment;

pub use affine::{Affine, Decomposed};
pub use arc::EllipseArc;
pub use bounds::Bounds;
pub use cubic::{ArcTable, CubicBez};
pub use line::Line;
pub use measure::PathMeasure;
pub use path::{FillRule, Path, PathEl, Subpath};
pub use point::Point;
pub use quad::QuadBez;
pub use segment::Segment;
