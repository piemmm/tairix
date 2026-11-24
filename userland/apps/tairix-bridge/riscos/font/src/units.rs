//! The units the Font Manager measures text in, each defined once.

/// Sixteenths of a point a point has: the unit a font's size is given in.
pub(crate) const SIXTEENTHS: i32 = 16;
/// Millipoints a point.
pub(crate) const MILLIPOINTS: i32 = 1000;
/// Points an inch.
pub(crate) const POINTS_AN_INCH: i32 = 72;
/// Millipoints an inch.
pub(crate) const MILLIPOINTS_AN_INCH: i32 = MILLIPOINTS * POINTS_AN_INCH;
/// Thousandths of an em an em has: what metrics and outlines measured in
/// ems are kept in.
pub(crate) const MILLEMS: i32 = 1000;
