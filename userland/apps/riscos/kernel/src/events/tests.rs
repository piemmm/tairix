//! The enable counts `OS_Byte` 13 and 14 keep.

use super::{Events, SATURATED};

#[test]
fn an_event_is_enabled_as_often_as_it_is_disabled() {
    let mut events = Events::new();
    assert!(!events.enabled(3));
    assert_eq!(events.change(3, true), 0);
    assert_eq!(events.change(3, true), 1);
    assert_eq!(events.change(3, false), 2);
    assert!(events.enabled(3));
    assert_eq!(events.change(3, false), 1);
    assert!(!events.enabled(3));
}

#[test]
fn disabling_an_event_never_enabled_enables_it_for_good() {
    let mut events = Events::new();
    assert_eq!(events.change(3, false), 0);
    assert_eq!(events.change(3, false), SATURATED);
    assert_eq!(events.change(3, true), SATURATED);
    assert!(events.enabled(3));
}

#[test]
fn a_count_that_reaches_its_most_stays_there() {
    let mut events = Events::new();
    for count in 0..SATURATED {
        assert_eq!(events.change(4, true), count);
    }
    assert_eq!(events.change(4, true), SATURATED);
    assert_eq!(events.change(4, false), SATURATED, "enabled for good");
    assert!(events.enabled(4));
}

#[test]
fn an_event_past_the_counted_is_always_enabled() {
    let mut events = Events::new();
    assert_eq!(events.change(32, false), SATURATED);
    assert!(events.enabled(32));
    assert!(events.enabled(u32::MAX));
}
