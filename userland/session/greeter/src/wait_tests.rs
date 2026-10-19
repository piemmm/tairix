use tairix_abi::time::Duration64;
use tairix_abi::WAITSET_TIMEOUT_NONE;
use tairix_theme::Timeline;

use super::{frame_budget, park_timeout, Idle, ENERGY_SAVING_AFTER_NS, NANOS_PER_SEC};

#[test]
fn a_frame_budget_covers_the_whole_span_and_one_frame_more() {
    let a_second = frame_budget(1_000);
    let cadences = u32::try_from(NANOS_PER_SEC / Timeline::FRAME_NS).expect("a small count");
    assert_eq!(a_second, cadences + 1, "the frame on the end is drawn too");

    // An animation the theme has done away with still leaves room for the
    // one frame that draws its finished state.
    assert_eq!(frame_budget(0), 1);

    // The longest span a theme can author is still a bounded number of
    // frames rather than an open loop.
    assert!(frame_budget(u16::MAX) > a_second);
}

#[test]
fn an_idle_screen_arms_no_timer() {
    assert_eq!(park_timeout(None, Duration64::ZERO), WAITSET_TIMEOUT_NONE);
}

#[test]
fn the_clock_alone_wakes_when_it_next_changes() {
    assert_eq!(
        park_timeout(Some(40 * NANOS_PER_SEC), Duration64::ZERO),
        40 * NANOS_PER_SEC
    );
}

#[test]
fn a_running_lockout_ticks_once_a_second() {
    let timeout = park_timeout(Some(40 * NANOS_PER_SEC), Duration64::from_secs(30));
    assert_eq!(timeout, NANOS_PER_SEC);
}

#[test]
fn the_last_of_a_lockout_wakes_exactly_when_it_ends() {
    let nearly_done = Duration64::new(0, 400_000_000).expect("a canonical field");
    let timeout = park_timeout(Some(40 * NANOS_PER_SEC), nearly_done);
    assert_eq!(timeout, 400_000_000);
}

#[test]
fn the_deadline_is_the_nearer_of_the_two() {
    let close_to_the_minute = Some(500_000_000);
    assert_eq!(
        park_timeout(close_to_the_minute, Duration64::from_secs(30)),
        500_000_000,
        "the clock is nearer"
    );
    assert_eq!(
        park_timeout(
            close_to_the_minute,
            Duration64::new(0, 100_000_000).expect("a canonical field"),
        ),
        100_000_000,
        "the lockout is nearer"
    );
}

#[test]
fn a_lockout_wakes_when_its_shown_second_turns_over() {
    let timeout = park_timeout(None, Duration64::from_nanos(2_900_000_000));
    assert_eq!(timeout, 900_000_000);
}

#[test]
fn a_lockout_with_no_clock_still_ticks() {
    let timeout = park_timeout(None, Duration64::from_secs(5));
    assert_eq!(timeout, NANOS_PER_SEC);
}

#[test]
fn the_display_sleeps_thirty_minutes_after_the_last_input() {
    assert_eq!(ENERGY_SAVING_AFTER_NS, 30 * 60 * NANOS_PER_SEC);
    let mut idle = Idle::new(0);
    assert_eq!(idle.timeout(0), ENERGY_SAVING_AFTER_NS);
    assert!(!idle.is_due(ENERGY_SAVING_AFTER_NS - 1));
    assert!(idle.is_due(ENERGY_SAVING_AFTER_NS));
    assert_eq!(idle.timeout(ENERGY_SAVING_AFTER_NS + 5), 0);

    idle.input(10 * NANOS_PER_SEC);
    assert!(
        !idle.is_due(ENERGY_SAVING_AFTER_NS),
        "input starts the wait again"
    );
    assert_eq!(idle.timeout(ENERGY_SAVING_AFTER_NS), 10 * NANOS_PER_SEC);
}

/// A clock that cannot run as far as the deadline still has one: the sum
/// saturates rather than wrapping to a deadline already past.
#[test]
fn a_wait_near_the_end_of_the_clock_saturates() {
    let idle = Idle::new(u64::MAX - 1);
    assert_eq!(idle.timeout(u64::MAX - 1), 1);
    assert!(!idle.is_due(u64::MAX - 1));
    assert!(idle.is_due(u64::MAX));
}
