use tairix_abi::session_ipc::SESSION_CLOSE_GRACE;

use super::Departure;

const START: u64 = 1_000;

fn grace() -> u64 {
    SESSION_CLOSE_GRACE.saturating_total_nanos()
}

#[test]
fn every_window_is_asked_once_including_one_opened_while_leaving() {
    let mut departure = Departure::begin(START, 0);
    assert_eq!(departure.unasked([3, 7]), [3, 7]);
    assert!(departure.unasked([3, 7]).is_empty(), "never asked twice");
    assert_eq!(
        departure.unasked([3, 7, 9]),
        [9],
        "a late window is asked in turn"
    );
}

#[test]
fn the_session_leaves_once_its_windows_are_closed_or_the_grace_is_spent() {
    let departure = Departure::begin(START, 103);
    assert!(!departure.is_complete(START, true));
    assert!(
        departure.is_complete(START, false),
        "nothing left to wait for"
    );
    assert!(!departure.is_complete(START + grace() - 1, true));
    assert!(
        departure.is_complete(START + grace(), true),
        "a window that will not close is left"
    );
    assert_eq!(departure.exit_code(), 103);
}

#[test]
fn the_grace_tightens_the_park_and_never_loosens_it() {
    let departure = Departure::begin(START, 0);
    assert_eq!(departure.park_deadline_ns(u64::MAX), START + grace());
    assert_eq!(departure.park_deadline_ns(START + 1), START + 1);
}

#[test]
fn a_departure_begun_near_the_clock_limit_still_ends() {
    let departure = Departure::begin(u64::MAX - 1, 0);
    assert!(departure.is_complete(u64::MAX, true));
}
