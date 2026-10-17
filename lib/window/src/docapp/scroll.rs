//! How a window showing part of a larger drawing — a dialog the desktop
//! fitted to a smaller screen — scrolls through the rest by the wheel.

use tairix_geometry::{Point, Scale};
use tairix_input::wheel::{wheel_steps, WHEEL_STEP};

/// Where a window scrolled to `at` within a drawing `room` pixels larger
/// than it, across then down, is scrolled to by a wheel turn of `units`
/// scroll units at `scale`, carrying what falls short of a pixel in `carry`.
/// Never past either end.
pub(crate) fn scrolled(
    at: Point,
    room: (u32, u32),
    units: (i32, i32),
    carry: &mut (i64, i64),
    scale: Scale,
) -> Point {
    let detent = u64::from(scale.scale_length(WHEEL_STEP).max(1));
    let step = |at: i32, units: i32, carry: &mut i64, room: u32| -> i32 {
        let moved = wheel_steps(units, detent, carry);
        let to = i64::from(at)
            .saturating_add(moved)
            .clamp(0, i64::from(room));
        i32::try_from(to).unwrap_or(i32::MAX)
    };
    Point::new(
        step(at.x, units.0, &mut carry.0, room.0),
        step(at.y, units.1, &mut carry.1, room.1),
    )
}

#[cfg(test)]
mod tests {
    use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

    use super::*;

    #[test]
    fn a_detent_scrolls_a_wheel_step_and_never_past_the_end() {
        let mut carry = (0, 0);
        let detent = (0, SCROLL_UNITS_PER_DETENT);
        let at = scrolled(Point::ORIGIN, (0, 100), detent, &mut carry, Scale::ONE);
        assert_eq!(at, Point::new(0, 48));
        let at = scrolled(at, (0, 100), detent, &mut carry, Scale::ONE);
        let at = scrolled(at, (0, 100), detent, &mut carry, Scale::ONE);
        assert_eq!(at, Point::new(0, 100), "held at the end");
        let back = (0, -SCROLL_UNITS_PER_DETENT * 4);
        assert_eq!(
            scrolled(at, (0, 100), back, &mut carry, Scale::ONE),
            Point::ORIGIN,
            "and at the start"
        );
    }

    #[test]
    fn a_drawing_that_fits_takes_no_scroll() {
        let mut carry = (0, 0);
        let turn = (SCROLL_UNITS_PER_DETENT, SCROLL_UNITS_PER_DETENT);
        assert_eq!(
            scrolled(Point::ORIGIN, (0, 0), turn, &mut carry, Scale::ONE),
            Point::ORIGIN
        );
    }
}
