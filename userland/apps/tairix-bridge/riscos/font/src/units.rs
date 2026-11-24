//! The units the Font Manager measures text in, signed as its arithmetic
//! takes them: RISC OS's own from `lib/drawing`, and the em's.

use tairix_drawing::riscos::units::{
    MILLIPOINTS_PER_INCH, MILLIPOINTS_PER_POINT, POINTS_PER_INCH, SIXTEENTHS_PER_POINT,
};

/// Sixteenths of a point a point has: the unit a font's size is given in.
pub(crate) const SIXTEENTHS: i32 = SIXTEENTHS_PER_POINT.cast_signed();
/// Millipoints a point.
pub(crate) const MILLIPOINTS: i32 = MILLIPOINTS_PER_POINT.cast_signed();
/// Points an inch.
pub(crate) const POINTS_AN_INCH: i32 = POINTS_PER_INCH.cast_signed();
/// Millipoints an inch.
pub(crate) const MILLIPOINTS_AN_INCH: i32 = MILLIPOINTS_PER_INCH.cast_signed();
/// Thousandths of an em an em has: what metrics and outlines measured in
/// ems are kept in.
pub(crate) const MILLEMS: i32 = 1000;
