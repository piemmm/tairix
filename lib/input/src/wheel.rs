//! How far a wheel turn scrolls: the one distance a detent moves every view,
//! and the one conversion from the seat's scroll units to a view's own steps.

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

/// How far one wheel detent scrolls a view, in logical pixels: about three
/// lines of body text.
///
/// One distance for every view rather than a count of each view's own rows,
/// so a detent moves a list of tall cards and a column of prose alike. The
/// seat has already accelerated a fast spin into more than one detent's
/// worth of scroll units.
pub const WHEEL_STEP: u32 = 48;

/// The steps `units` of scroll move a view that moves `per_detent` steps a
/// wheel detent — pixels for a scroll view, whole tools for a strip —
/// carrying what is short of a whole step in `carry`.
///
/// The one conversion from the seat's scroll units, so a turn moves every
/// view in proportion to it. A reversal drops the carry, so a turn back is
/// never shortened by what the turn before it left over.
pub fn wheel_steps(units: i32, per_detent: u64, carry: &mut i64) -> i64 {
    if units == 0 {
        return 0;
    }
    if (*carry < 0) != (units < 0) {
        *carry = 0;
    }
    let per = i128::from(SCROLL_UNITS_PER_DETENT);
    let step = i64::try_from(per_detent).unwrap_or(i64::MAX);
    let total = i128::from(*carry) + i128::from(units) * i128::from(step);
    let moved = total / per;
    *carry = i64::try_from(total - moved * per).unwrap_or(0);
    i64::try_from(moved).unwrap_or(if moved < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detent_moves_one_detents_steps_and_a_fraction_is_carried() {
        let mut carry = 0;
        assert_eq!(wheel_steps(SCROLL_UNITS_PER_DETENT, 48, &mut carry), 48);
        assert_eq!(carry, 0);
        let half = SCROLL_UNITS_PER_DETENT / 2;
        assert_eq!(wheel_steps(half, 3, &mut carry), 1, "one and a half steps");
        assert_eq!(wheel_steps(half, 3, &mut carry), 2, "and the half carried");
        assert_eq!(carry, 0);
    }

    #[test]
    fn a_reversal_drops_the_carry_and_nothing_moves_nothing() {
        let mut carry = 0;
        assert_eq!(wheel_steps(SCROLL_UNITS_PER_DETENT / 4, 2, &mut carry), 0);
        assert!(carry > 0);
        assert_eq!(wheel_steps(-SCROLL_UNITS_PER_DETENT, 2, &mut carry), -2);
        assert_eq!(wheel_steps(0, 2, &mut carry), 0);
    }

    #[test]
    fn a_vast_turn_saturates_rather_than_wrapping() {
        let mut carry = 0;
        assert_eq!(wheel_steps(i32::MAX, u64::MAX, &mut carry), i64::MAX);
        let mut carry = 0;
        assert_eq!(wheel_steps(i32::MIN, u64::MAX, &mut carry), i64::MIN);
    }
}
