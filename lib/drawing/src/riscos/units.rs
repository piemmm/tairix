//! RISC OS's units, each defined once: the draw unit a Draw file and the
//! Draw module measure in, the OS unit the screen is measured in, and the
//! point, which the Font Manager places text in thousandths of and sizes it
//! in sixteenths of. A unit that follows from others is derived from them.

pub use tairix_image::OS_UNITS_PER_INCH;

pub use crate::drawing::POINTS_PER_INCH;

/// Draw units to the point.
pub const DRAW_UNITS_PER_POINT: u32 = 640;

/// Draw units to the inch.
pub const DRAW_UNITS_PER_INCH: u32 = DRAW_UNITS_PER_POINT * POINTS_PER_INCH;

/// Draw units to the OS unit.
pub const DRAW_UNITS_PER_OS_UNIT: u32 = DRAW_UNITS_PER_INCH / OS_UNITS_PER_INCH;

/// [`DRAW_UNITS_PER_OS_UNIT`] as a shift.
pub const DRAW_UNIT_SHIFT: u32 = DRAW_UNITS_PER_OS_UNIT.trailing_zeros();

/// Millipoints to the point.
pub const MILLIPOINTS_PER_POINT: u32 = 1000;

/// Millipoints to the inch.
pub const MILLIPOINTS_PER_INCH: u32 = MILLIPOINTS_PER_POINT * POINTS_PER_INCH;

/// Millipoints to the OS unit: the scale the Font Manager starts at.
pub const MILLIPOINTS_PER_OS_UNIT: u32 = MILLIPOINTS_PER_INCH / OS_UNITS_PER_INCH;

/// Sixteenths of a point to the point: the unit a font's size is given in.
pub const SIXTEENTHS_PER_POINT: u32 = 16;

// The divisions above are exact, and the shift is the ratio it stands for.
const _: () = assert!(DRAW_UNITS_PER_INCH.is_multiple_of(OS_UNITS_PER_INCH));
const _: () = assert!(MILLIPOINTS_PER_INCH.is_multiple_of(OS_UNITS_PER_INCH));
const _: () = assert!(DRAW_UNITS_PER_OS_UNIT.is_power_of_two());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_units_are_risc_os_s() {
        assert_eq!(OS_UNITS_PER_INCH, 180);
        assert_eq!(DRAW_UNITS_PER_OS_UNIT, 256);
        assert_eq!(DRAW_UNIT_SHIFT, 8);
        assert_eq!(MILLIPOINTS_PER_OS_UNIT, 400);
    }
}
