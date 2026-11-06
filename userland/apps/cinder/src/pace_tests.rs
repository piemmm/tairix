use super::*;

const FRAME: u64 = 16_666_667;

#[test]
fn a_moving_companion_wakes_every_frame() {
    let pace = Pace {
        moving: true,
        unshown: false,
        reconsider_in: 2.0,
    };
    assert_eq!(pace.wake_at(1_000, FRAME), 1_000 + FRAME);
}

/// A companion let out asleep is still shown: his first picture is owed at
/// the next frame, whatever his mind is doing.
#[test]
fn one_let_out_owes_his_first_picture_at_the_next_frame() {
    let pace = Pace {
        moving: false,
        unshown: true,
        reconsider_in: 2.5,
    };
    assert_eq!(pace.wake_at(0, FRAME), FRAME);
}

/// A nap wakes him when his mind next reconsiders, so it ends; it never
/// leaves him with no deadline at all.
#[test]
fn a_still_companion_wakes_when_his_mind_reconsiders() {
    let pace = Pace {
        moving: false,
        unshown: false,
        reconsider_in: 1.5,
    };
    assert_eq!(pace.wake_at(7, FRAME), 7 + 1_500_000_000);
    let due = Pace {
        reconsider_in: 0.0,
        ..pace
    };
    assert_eq!(
        due.wake_at(7, FRAME),
        7 + FRAME,
        "never sooner than a frame"
    );
}

/// A wake advances him by the time that passed, so a nap's seconds are slept
/// through; a stall is held to a few frames while he moves.
#[test]
fn a_wake_advances_by_what_passed_within_its_bound() {
    let still = Pace {
        moving: false,
        unshown: false,
        reconsider_in: 2.5,
    };
    assert!((still.step(2_500_000_000, FRAME) - 2.5).abs() < 1e-9);
    assert!((still.step(60_000_000_000, FRAME) - RECONSIDER_SECONDS).abs() < 1e-9);
    let moving = Pace {
        moving: true,
        ..still
    };
    assert!((moving.step(FRAME, FRAME) - seconds(FRAME)).abs() < 1e-9);
    assert!((moving.step(10_000_000_000, FRAME) - seconds(4 * FRAME)).abs() < 1e-9);
}
