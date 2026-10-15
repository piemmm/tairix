use super::{NETMAP_RANGE_END, NETMAP_RANGE_START, PROBE_FINISHED, PROBE_REFUSED, PROBE_STARTED};
use tairix_log::EventId;

#[test]
fn the_event_ids_are_frozen_and_in_range() {
    assert_eq!(PROBE_STARTED, EventId(33_001));
    assert_eq!(PROBE_FINISHED, EventId(33_002));
    assert_eq!(PROBE_REFUSED, EventId(33_003));
    for id in [PROBE_STARTED, PROBE_FINISHED, PROBE_REFUSED] {
        assert!((NETMAP_RANGE_START..NETMAP_RANGE_END).contains(&id.0));
    }
}
