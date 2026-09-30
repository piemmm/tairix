//! Host tests of the ribbon's clock: the frames it asks for, how far each
//! moves it, and that a still ribbon asks for none.

use super::{seconds, Motion, FRAME_NS, MOST_FRAMES};

#[test]
fn a_moving_ribbon_asks_for_a_frame_a_frame_on() {
    let mut motion = Motion::new(0, false);
    assert_eq!(motion.due_ns(), Some(FRAME_NS));
    assert!(
        !motion.frame_due(FRAME_NS - 1),
        "an early wake draws nothing"
    );
    assert!(motion.frame_due(FRAME_NS));

    let moved = motion.advance(FRAME_NS);
    assert!((moved - seconds(FRAME_NS)).abs() < 1e-12, "{moved} s moved");
    assert_eq!(motion.due_ns(), Some(2 * FRAME_NS));
}

#[test]
fn a_still_ribbon_asks_for_nothing_and_never_moves() {
    let mut motion = Motion::new(0, true);
    assert_eq!(motion.due_ns(), None);
    assert!(!motion.frame_due(u64::MAX));
    assert!(motion.advance(u64::MAX).abs() < f64::EPSILON);
    assert_eq!(motion.due_ns(), None);
}

/// A wake that came late — a busy machine, or a screen coming back from
/// sleep — moves the ribbon a few frames on, never all the way to the clock.
#[test]
fn a_late_wake_moves_the_ribbon_no_more_than_a_few_frames() {
    let mut motion = Motion::new(0, false);
    motion.advance(FRAME_NS);
    let late = 50 * 1_000_000_000;
    let moved = motion.advance(late);
    let most = seconds((1 + MOST_FRAMES) * FRAME_NS);
    assert!((moved - most).abs() < 1e-9, "{moved} s moved");
    assert_eq!(motion.due_ns(), Some(late + FRAME_NS));
}

/// Time only ever runs forward: a clock that stepped back moves nothing.
#[test]
fn a_clock_that_ran_backwards_moves_nothing() {
    let mut motion = Motion::new(10 * FRAME_NS, false);
    let moved = motion.advance(FRAME_NS);
    assert!(moved.abs() < f64::EPSILON, "{moved} s moved");
}
